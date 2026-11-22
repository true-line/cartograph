//! Calls and publishes that leave the system boundary (#1788, AC-0660,
//! ADR-0063, SPEC-13; catalogue D6 §6.5 and D7 §7.6): `external.md`.
//!
//! A deterministic projection of T0 and T1 facts. It writes none, changes no
//! tier and calls no model. Three fact kinds are read: the outbound HTTP
//! call that matched no endpoint (a `gap:fetch:` Gap), the observed client
//! (a `Resource` with `observed: true`, reached by `USES`) and the `Channel`
//! with its `PUBLISHES`, `SUBSCRIBES` and `BACKS` relations.
//!
//! **One predicate.** [`boundary`] decides every target. It is a pure
//! function of the target's own fact and of the origins that binding
//! citations in the graph name.
//!
//! **What the graph holds about declared bindings.** The system manifest is
//! read at ingest and is not stored, so this crate sees a declared binding
//! only where cross-repository HTTP binding recorded it on a fact it
//! evaluated (#1843): the `binding` citations of a joined `FETCHES`
//! relation, of a kept Gap and of an observed HTTP client, the `candidates`
//! of an ambiguous Gap, and the `binding_state` of every Gap that pass
//! kept. A Gap stored before the state was recorded carries `t0_reason`
//! and a sentence; the sentence is not parsed. Such a call is unclassified
//! with that stated, unless a citation elsewhere names its origin.
//!
//! **Outside is claimed only from a literal.** The origin must be written in
//! the source (`url_source` `literal`, or a literal base joined to a literal
//! path). Origins are compared as scheme, host and port, with scheme and
//! host in lower case as HTTP binding compares them.
//!
//! **Cost.** One index over the admitted facts, one pass over the flow hops,
//! and for each flow that reaches an outside target one pass over its hops:
//! linear in facts plus hops. Nothing is walked.
//!
//! **Mode.** The producer receives the facts the export mode admitted, so a
//! `verified-only` export holds no InferredWeak row. The caller counts what
//! the mode and the rejections withheld ([`withheld`]) and the document
//! states it (R-INT-5).

use super::producers::Output;
use super::render::{Assertions, Budget, Literal, NodeName, RenderLimit, Text};
use super::{ExportMode, edge_identity, included, provenance};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, Provenance};
use flowtracer::Flow;
use serde::Deserialize;
use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

const FETCH_GAP: &str = "gap:fetch:";
const OBSERVED_GAP: &str = "gap:observed:";
const HTTP_API: &str = "http-api";
const IN_PROCESS: &str = "inproc-event";
/// What cross-repository HTTP binding recorded about a kept Gap's origin
/// (`events::BINDING_STATE` and its values; this crate does not depend on
/// `events`, and a test there and here pins the same words).
const BINDING_STATE: &str = "binding_state";
/// No declared binding names the origin.
const STATE_NONE: &str = "none";
/// A declared binding names the origin, or places the call in a registered
/// repository, or several endpoints are equally supported.
const STATES_INSIDE: [&str; 3] = ["origin_declared", "names_repository", "ambiguous"];

/// Why a target is listed as unclassified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Unknown {
    /// No request was recovered, or its origin is not written at the call.
    HostNotInSource,
    /// The URL names a host and no scheme (`//host/path`).
    SchemeNotInSource,
    /// The binding pass kept the Gap before it recorded its outcome as a
    /// field, so the outcome is a sentence only.
    BindingNotRecorded,
    /// The method is computed and no binding pass recorded what the
    /// declared bindings say about the origin.
    MethodNotKnown,
    /// An SDK or auth-provider client.
    ProviderOperator,
    /// A published channel with no subscriber and no backing resource.
    NoSubscriber,
}

impl Unknown {
    fn reason(self) -> &'static str {
        match self {
            Self::HostNotInSource => "host not known from source",
            Self::SchemeNotInSource => "the URL names a host without a scheme",
            Self::BindingNotRecorded => {
                "binding evidence was evaluated for this call, and the fact does not record \
                 whether a declared binding covers its host"
            }
            Self::MethodNotKnown => {
                "the method is computed at run time, so binding evidence was not evaluated for \
                 this call"
            }
            Self::ProviderOperator => "the registries do not record who operates this provider",
            Self::NoSubscriber => "no subscriber or backing resource recovered",
        }
    }
}

/// Why a target does not cross the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NotCrossing {
    /// Binding evidence places the call in a registered repository and no
    /// one endpoint there was matched. Counted, never listed as a target.
    InsideUnresolved,
    /// A URL without a host: an unresolved call to the caller's own origin.
    SameOrigin,
    /// Browser storage.
    BrowserStorage,
    /// An in-process event channel.
    InProcess,
    /// A channel with no recovered publisher (inbound).
    Inbound,
}

/// Where a target stands against the system boundary (ADR-0063).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Boundary {
    Inside,
    Outside,
    Unclassified(Unknown),
    NotACrossing(NotCrossing),
}

/// One fact the predicate decides, with what the graph relates to it.
#[derive(Clone, Copy)]
pub(super) enum Target<'a> {
    /// A `gap:fetch:` Gap.
    Fetch(&'a Node),
    /// A `Resource` with `observed: true`.
    Observed(&'a Node),
    /// A `gap:observed:` Gap: a client whose instance is computed.
    ObservedGap(&'a Node),
    /// A `Channel` and how many relations of each kind reach it.
    Channel {
        node: &'a Node,
        publishers: usize,
        subscribers: usize,
        backing: usize,
    },
}

/// What a URL says about its host.
enum Host {
    /// `scheme://host[:port]`, scheme and host in lower case, no userinfo.
    Origin(String),
    /// `//host/path`.
    NoScheme,
    /// A path: the caller's own origin.
    Same,
}

fn host(url: &str) -> Host {
    let url = url.trim();
    if url.starts_with("//") {
        return Host::NoScheme;
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return Host::Same;
    };
    // `/next?to=https://x` is a path, not a URL with a scheme.
    let valid = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid {
        return Host::Same;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = authority.rsplit('@').next().unwrap_or(authority);
    if host.is_empty() {
        return Host::NoScheme;
    }
    Host::Origin(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        host.to_ascii_lowercase()
    ))
}

fn origin(url: &str) -> Option<String> {
    match host(url) {
        Host::Origin(origin) => Some(origin),
        Host::NoScheme | Host::Same => None,
    }
}

/// The origin a target names with a literal, when it names one.
fn target_origin(target: &Target<'_>) -> Option<String> {
    match target {
        Target::Fetch(gap) => origin(gap.props["request"]["url"].as_str()?),
        Target::Observed(resource) => origin(resource.props["identity"].as_str()?),
        Target::ObservedGap(_) | Target::Channel { .. } => None,
    }
}

/// Whether the origin of a recovered request is written in the source: the
/// URL is a literal, or a literal base joined to a literal path.
fn literal_source(request: &serde_json::Value) -> bool {
    matches!(
        request["url_source"].as_str(),
        Some("literal" | "base:literal")
    )
}

/// The one boundary predicate (ADR-0063 decision 5, SPEC-13). `bound` holds
/// the origins that binding citations in the graph name. The first matching
/// row applies; nothing is stored.
pub(super) fn boundary(target: &Target<'_>, bound: &BTreeSet<String>) -> Boundary {
    use Boundary::{Inside, NotACrossing, Outside, Unclassified};
    match target {
        Target::Fetch(gap) => {
            let request = &gap.props["request"];
            // Several equally supported targets, each in a registered
            // repository.
            let state = gap.props[BINDING_STATE].as_str();
            if gap.props["candidates"]
                .as_array()
                .is_some_and(|candidates| !candidates.is_empty())
                || state.is_some_and(|state| STATES_INSIDE.contains(&state))
            {
                return NotACrossing(NotCrossing::InsideUnresolved);
            }
            let Some(url) = request["url"].as_str() else {
                return Unclassified(Unknown::HostNotInSource);
            };
            match host(url) {
                Host::Same => NotACrossing(NotCrossing::SameOrigin),
                Host::NoScheme => Unclassified(Unknown::SchemeNotInSource),
                Host::Origin(origin) if bound.contains(&origin) => {
                    NotACrossing(NotCrossing::InsideUnresolved)
                }
                Host::Origin(_) if !literal_source(request) => {
                    Unclassified(Unknown::HostNotInSource)
                }
                // The binding pass evaluated this call and no declared
                // binding names its origin (#1843).
                Host::Origin(_) if state == Some(STATE_NONE) => Outside,
                // The binding pass kept this Gap before it recorded its
                // outcome as a field. Whether it found no binding, or one
                // with no matching endpoint, is a sentence only.
                Host::Origin(_) if gap.props["t0_reason"].is_string() => {
                    Unclassified(Unknown::BindingNotRecorded)
                }
                // A computed method, and no pass recorded a binding state.
                Host::Origin(_) if request["method"].as_str().is_none_or(|m| m == "?") => {
                    Unclassified(Unknown::MethodNotKnown)
                }
                Host::Origin(_) => Outside,
            }
        }
        Target::Observed(resource) => match resource.props["kind"].as_str() {
            Some("browser-storage") => NotACrossing(NotCrossing::BrowserStorage),
            Some(HTTP_API) => match target_origin(target) {
                Some(origin) if bound.contains(&origin) => Inside,
                Some(_) => Outside,
                None => Unclassified(Unknown::HostNotInSource),
            },
            // `sdk-client`, `auth-provider`, and any family added later.
            _ => Unclassified(Unknown::ProviderOperator),
        },
        Target::ObservedGap(gap) => match gap.props["kind"].as_str() {
            Some("browser-storage") => NotACrossing(NotCrossing::BrowserStorage),
            Some(HTTP_API) => Unclassified(Unknown::HostNotInSource),
            _ => Unclassified(Unknown::ProviderOperator),
        },
        Target::Channel {
            node,
            publishers,
            subscribers,
            backing,
        } => {
            if node.props["kind"] == IN_PROCESS {
                NotACrossing(NotCrossing::InProcess)
            } else if *publishers == 0 {
                NotACrossing(NotCrossing::Inbound)
            } else if *subscribers > 0 || *backing > 0 {
                Inside
            } else {
                Unclassified(Unknown::NoSubscriber)
            }
        }
    }
}

fn is_fetch_gap(node: &Node) -> bool {
    node.label == "Gap" && node.id.starts_with(FETCH_GAP)
}

fn is_observed_gap(node: &Node) -> bool {
    node.label == "Gap" && node.id.starts_with(OBSERVED_GAP)
}

fn is_observed(node: &Node) -> bool {
    node.label == "Resource" && node.props["observed"] == true
}

/// How many boundary facts the export mode and the rejections withhold,
/// counted over the facts before that filter: outbound-call Gaps, observed
/// clients and channels, and the relations that reach them.
pub(super) fn withheld<N: Borrow<Node>, E: Borrow<Edge>>(
    nodes: &[N],
    edges: &[E],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
) -> usize {
    let nodes = nodes
        .iter()
        .map(Borrow::borrow)
        .filter(|node| {
            is_fetch_gap(node)
                || is_observed_gap(node)
                || is_observed(node)
                || node.label == "Channel"
        })
        .filter(|node| !included(&provenance(&node.props, &node.id), mode, rejected))
        .count();
    let edges = edges
        .iter()
        .map(Borrow::borrow)
        .filter(|edge| match edge.label.as_str() {
            "FETCHES" => edge.dst.starts_with(FETCH_GAP),
            "USES" => edge.dst.contains("@observed:") || edge.dst.starts_with(OBSERVED_GAP),
            "PUBLISHES" | "SUBSCRIBES" | "BACKS" => edge.dst.starts_with("chan:"),
            _ => false,
        })
        .filter(|edge| {
            !included(
                &provenance(&edge.props, &edge_identity(edge)),
                mode,
                rejected,
            )
        })
        .count();
    nodes + edges
}

/// Repository, path and first byte of one evidence span: one call site.
type Place<'a> = (&'a str, &'a str, u64);

/// The evidence spans a fact stores, without decoding its provenance.
fn places(props: &serde_json::Value) -> impl Iterator<Item = Place<'_>> {
    crate::workbench::stored_spans(props)
        .into_iter()
        .filter_map(|span| Some((span.repo?, span.path?, span.byte_start?)))
}

fn confidence(props: &serde_json::Value) -> ConfidenceTier {
    ConfidenceTier::deserialize(&crate::stored_prov(props)["confidence_tier"])
        .unwrap_or(ConfidenceTier::Gap)
}

fn rank(confidence: ConfidenceTier) -> u8 {
    match confidence {
        ConfidenceTier::Confirmed => 3,
        ConfidenceTier::InferredStrong => 2,
        ConfidenceTier::InferredWeak => 1,
        ConfidenceTier::Gap => 0,
    }
}

/// One fact a row rests on.
#[derive(Clone, Copy)]
pub(super) enum Fact<'a> {
    Node(&'a Node),
    Edge(&'a Edge),
}

impl Fact<'_> {
    fn props(&self) -> &serde_json::Value {
        match self {
            Self::Node(node) => &node.props,
            Self::Edge(edge) => &edge.props,
        }
    }

    fn provenance(&self) -> Provenance {
        match self {
            Self::Node(node) => provenance(&node.props, &node.id),
            Self::Edge(edge) => provenance(&edge.props, &edge_identity(edge)),
        }
    }
}

/// The fact with the lowest confidence among those a row rests on, so the
/// row never states a higher one. The first wins a tie.
fn lowest<'a>(facts: impl Iterator<Item = Fact<'a>>) -> Option<Fact<'a>> {
    facts.fold(None, |lowest: Option<Fact<'a>>, fact| match lowest {
        Some(held) if rank(confidence(held.props())) <= rank(confidence(fact.props())) => {
            Some(held)
        }
        _ => Some(fact),
    })
}

/// One call or use of an outside origin.
pub(super) struct Site<'a> {
    /// The outbound-call Gap, when the call is one.
    pub(super) gap: Option<&'a Node>,
    /// The `FETCHES` relation into that Gap.
    pub(super) call: Option<&'a Edge>,
    /// The observed use: of the same call, or on its own.
    pub(super) uses: Option<&'a Edge>,
    /// The observed `http-api` resource of that use.
    pub(super) resource: Option<&'a Node>,
    /// Where the site is, for the order of the rows.
    at: Option<Place<'a>>,
}

impl<'a> Site<'a> {
    pub(super) fn facts(&self) -> impl Iterator<Item = Fact<'a>> + use<'a> {
        [
            self.gap.map(Fact::Node),
            self.call.map(Fact::Edge),
            self.uses.map(Fact::Edge),
            self.resource.map(Fact::Node),
        ]
        .into_iter()
        .flatten()
    }

    pub(super) fn caller(&self) -> Option<&'a str> {
        self.call
            .or(self.uses)
            .map(|edge| edge.src.as_str())
            .filter(|src| !src.is_empty())
    }
}

/// The kind of an unclassified target, in the order the rows are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Call,
    HttpClient,
    SdkClient,
    AuthProvider,
    Client,
    Channel,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Call => "Outbound HTTP call",
            Self::HttpClient => "HTTP client",
            Self::SdkClient => "SDK client",
            Self::AuthProvider => "Auth-provider client",
            Self::Client => "Observed client",
            Self::Channel => "Published channel",
        }
    }
}

/// One unclassified target.
pub(super) struct Row<'a> {
    kind: Kind,
    pub(super) node: &'a Node,
    reason: Unknown,
    /// The relations that reach it: its call, its uses or its publishers.
    pub(super) edges: Vec<&'a Edge>,
}

impl<'a> Row<'a> {
    /// An outbound HTTP call or HTTP client whose host is not known.
    pub(super) fn is_http(&self) -> bool {
        matches!(self.kind, Kind::Call | Kind::HttpClient)
    }
    /// Why the row is unclassified, as `external.md` states it.
    pub(super) fn reason_text(&self) -> &'static str {
        self.reason.reason()
    }
    fn facts(&self) -> impl Iterator<Item = Fact<'a>> + '_ {
        std::iter::once(Fact::Node(self.node)).chain(self.edges.iter().copied().map(Fact::Edge))
    }
}

/// The admitted facts as the document lists them.
pub(super) struct Model<'a> {
    /// Outside targets by origin, each with its sites.
    pub(super) outside: BTreeMap<String, Vec<Site<'a>>>,
    pub(super) unclassified: Vec<Row<'a>>,
    /// Calls binding evidence places in a registered repository.
    inside_unresolved: usize,
    /// Unresolved calls to the caller's own origin.
    same_origin: usize,
    /// Published channels with a subscriber or a backing resource.
    inside_channels: usize,
    /// `READS` and `WRITES` relations from a Symbol to a DataEntity, by
    /// Symbol.
    access: BTreeMap<&'a str, Vec<&'a Edge>>,
    by_id: BTreeMap<&'a str, &'a Node>,
}

fn sorted<'a>(edges: Option<&Vec<&'a Edge>>) -> Vec<&'a Edge> {
    let mut edges = edges.cloned().unwrap_or_default();
    edges.sort_by(|left, right| left.src.cmp(&right.src));
    edges.dedup_by(|left, right| left.src == right.src);
    edges
}

impl<'a> Model<'a> {
    pub(super) fn new(nodes: &[&'a Node], edges: &[&'a Edge]) -> Self {
        // Never the store's or the caller's order, and each fact once.
        let by_id: BTreeMap<&str, &Node> =
            nodes.iter().map(|node| (node.id.as_str(), *node)).collect();
        let label = |id: &str| by_id.get(id).map(|node| node.label.as_str());

        // Origins a binding citation names, and the relations by target.
        let mut bound = BTreeSet::new();
        let mut cite = |binding: &serde_json::Value| {
            for citation in binding.as_array().into_iter().flatten() {
                if let Some(origin) = citation["base_url"].as_str().and_then(origin) {
                    bound.insert(origin);
                }
            }
        };
        let mut calls: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
        let mut uses: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
        let mut publishes: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
        let mut subscribers: BTreeMap<&str, usize> = BTreeMap::new();
        let mut backing: BTreeMap<&str, usize> = BTreeMap::new();
        let mut access: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
        // Call sites that matched an endpoint of a registered repository.
        let mut resolved: BTreeSet<Place<'_>> = BTreeSet::new();
        for edge in edges.iter().copied() {
            let dst = edge.dst.as_str();
            match edge.label.as_str() {
                "FETCHES" => {
                    cite(&edge.props["binding"]);
                    if dst.starts_with(FETCH_GAP) {
                        calls.entry(dst).or_default().push(edge);
                    } else if !dst.starts_with("gap:") {
                        resolved.extend(places(&edge.props));
                    }
                }
                "USES" => uses.entry(dst).or_default().push(edge),
                "PUBLISHES" => publishes.entry(dst).or_default().push(edge),
                "SUBSCRIBES" => *subscribers.entry(dst).or_default() += 1,
                "BACKS" => *backing.entry(dst).or_default() += 1,
                "READS" | "WRITES"
                    if label(&edge.src) == Some("Symbol") && label(dst) == Some("DataEntity") =>
                {
                    access.entry(edge.src.as_str()).or_default().push(edge);
                }
                _ => {}
            }
        }
        // One outbound call by its site, to find the observed use of it.
        let mut gap_at: BTreeMap<Place<'_>, &str> = BTreeMap::new();
        // An observed HTTP client of a declared host cites its bindings.
        for resource in by_id.values().copied().filter(|node| is_observed(node)) {
            cite(&resource.props["binding"]);
        }
        for gap in by_id.values().copied().filter(|node| is_fetch_gap(node)) {
            cite(&gap.props["binding"]);
            for candidate in gap.props["candidates"].as_array().into_iter().flatten() {
                cite(&candidate["binding"]);
            }
            if let Some(place) = places(&gap.props).next() {
                gap_at.entry(place).or_insert(gap.id.as_str());
            }
        }
        for edges in access.values_mut() {
            edges.sort_by(|left, right| (&left.dst, &left.label).cmp(&(&right.dst, &right.label)));
        }

        let mut model = Self {
            outside: BTreeMap::new(),
            unclassified: Vec::new(),
            inside_unresolved: 0,
            same_origin: 0,
            inside_channels: 0,
            access,
            by_id,
        };

        // Observed clients. A use at the site of an outbound call is that
        // call, and a use at the site of a call that matched an endpoint is
        // inside: neither is a site of its own.
        let mut twin: BTreeMap<&str, (&Edge, &Node)> = BTreeMap::new();
        for resource in model.by_id.values().copied() {
            if !is_observed(resource) {
                continue;
            }
            let target = Target::Observed(resource);
            let class = boundary(&target, &bound);
            let all = sorted(uses.get(resource.id.as_str()));
            if resource.props["kind"] != HTTP_API {
                if let Boundary::Unclassified(reason) = class {
                    model.unclassified.push(Row {
                        kind: match resource.props["kind"].as_str() {
                            Some("sdk-client") => Kind::SdkClient,
                            Some("auth-provider") => Kind::AuthProvider,
                            _ => Kind::Client,
                        },
                        node: resource,
                        reason,
                        edges: all,
                    });
                }
                continue;
            }
            let mut own = Vec::new();
            for edge in all {
                let mut accounted = false;
                let mut free = None;
                for place in places(&edge.props) {
                    if let Some(gap) = gap_at.get(&place) {
                        twin.entry(*gap).or_insert((edge, resource));
                        accounted = true;
                    } else if resolved.contains(&place) {
                        accounted = true;
                    } else if free.is_none() {
                        free = Some(place);
                    }
                }
                // A followed identity adds the span of its definition to
                // the use: beside an accounted call it is not a second call.
                let defined_elsewhere = edge.props["identity_from"].is_string();
                if !(accounted && (free.is_none() || defined_elsewhere)) {
                    own.push((edge, free));
                }
            }
            match class {
                Boundary::Outside => {
                    let origin = target_origin(&target).expect("an outside target has an origin");
                    model
                        .outside
                        .entry(origin)
                        .or_default()
                        .extend(own.into_iter().map(|(edge, at)| Site {
                            gap: None,
                            call: None,
                            uses: Some(edge),
                            resource: Some(resource),
                            at,
                        }));
                }
                Boundary::Unclassified(reason) if !own.is_empty() => {
                    model.unclassified.push(Row {
                        kind: Kind::HttpClient,
                        node: resource,
                        reason,
                        edges: own.into_iter().map(|(edge, _)| edge).collect(),
                    });
                }
                _ => {}
            }
        }

        for node in model.by_id.values().copied() {
            if is_fetch_gap(node) {
                let target = Target::Fetch(node);
                let call = sorted(calls.get(node.id.as_str()));
                match boundary(&target, &bound) {
                    Boundary::Outside => {
                        let origin =
                            target_origin(&target).expect("an outside target has an origin");
                        let observed = twin.get(node.id.as_str());
                        model.outside.entry(origin).or_default().push(Site {
                            gap: Some(node),
                            call: call.first().copied(),
                            uses: observed.map(|(edge, _)| *edge),
                            resource: observed.map(|(_, resource)| *resource),
                            at: places(&node.props).next(),
                        });
                    }
                    Boundary::Unclassified(reason) => model.unclassified.push(Row {
                        kind: Kind::Call,
                        node,
                        reason,
                        edges: call,
                    }),
                    Boundary::NotACrossing(NotCrossing::InsideUnresolved) => {
                        model.inside_unresolved += 1;
                    }
                    Boundary::NotACrossing(NotCrossing::SameOrigin) => model.same_origin += 1,
                    Boundary::Inside | Boundary::NotACrossing(_) => {}
                }
            } else if is_observed_gap(node) {
                // The same call as an outbound-call Gap is listed there.
                let same_call = places(&node.props)
                    .next()
                    .is_some_and(|place| gap_at.contains_key(&place));
                if node.props["kind"] == HTTP_API
                    && !same_call
                    && let Boundary::Unclassified(reason) =
                        boundary(&Target::ObservedGap(node), &bound)
                {
                    model.unclassified.push(Row {
                        kind: Kind::HttpClient,
                        node,
                        reason,
                        edges: sorted(uses.get(node.id.as_str())),
                    });
                }
            } else if node.label == "Channel" {
                let id = node.id.as_str();
                let publishers = sorted(publishes.get(id));
                let target = Target::Channel {
                    node,
                    publishers: publishers.len(),
                    subscribers: subscribers.get(id).copied().unwrap_or(0),
                    backing: backing.get(id).copied().unwrap_or(0),
                };
                match boundary(&target, &bound) {
                    Boundary::Inside => model.inside_channels += 1,
                    Boundary::Unclassified(reason) => model.unclassified.push(Row {
                        kind: Kind::Channel,
                        node,
                        reason,
                        edges: publishers,
                    }),
                    Boundary::Outside | Boundary::NotACrossing(_) => {}
                }
            }
        }

        for sites in model.outside.values_mut() {
            sites.sort_by(|left, right| {
                (left.at, left.caller(), left.gap.map(|gap| &gap.id)).cmp(&(
                    right.at,
                    right.caller(),
                    right.gap.map(|gap| &gap.id),
                ))
            });
        }
        model.outside.retain(|_, sites| !sites.is_empty());
        model
            .unclassified
            .sort_by(|left, right| (left.kind, &left.node.id).cmp(&(right.kind, &right.node.id)));
        model
    }
}

/// How one flow uses one data entity.
#[derive(Default, Clone, Copy)]
struct Access<'a> {
    read: Option<&'a Edge>,
    written: Option<&'a Edge>,
}

/// The flows, in trigger order, and which of them reach what.
struct Flows<'a> {
    /// In trigger order, so the rows do not depend on the caller's order.
    flows: Vec<&'a Flow>,
    /// Flows with a hop that ends at an outbound-call Gap, by Gap.
    reach: BTreeMap<&'a str, Vec<usize>>,
    /// Flows with a non-Gap hop at a Symbol, by Symbol, for the Symbols
    /// that hold an observed use.
    through: BTreeMap<&'a str, Vec<usize>>,
}

impl<'a> Flows<'a> {
    fn new(flows: &'a [Flow], model: &Model<'_>) -> Self {
        let users: BTreeSet<&str> = model
            .outside
            .values()
            .flatten()
            .filter(|site| site.gap.is_none())
            .filter_map(Site::caller)
            .collect();
        let mut flows: Vec<&Flow> = flows.iter().collect();
        flows.sort_by(|left, right| {
            (&left.trigger, &left.trigger_kind).cmp(&(&right.trigger, &right.trigger_kind))
        });
        let mut reach: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        let mut through: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (index, flow) in flows.iter().enumerate() {
            for hop in &flow.hops {
                if hop.dst.starts_with(FETCH_GAP) {
                    reach.entry(hop.dst.as_str()).or_default().push(index);
                }
                if hop.provenance.confidence_tier == ConfidenceTier::Gap {
                    continue;
                }
                for end in [hop.src.as_str(), hop.dst.as_str()] {
                    if users.contains(end) {
                        through.entry(end).or_default().push(index);
                    }
                }
            }
        }
        for list in reach.values_mut().chain(through.values_mut()) {
            list.dedup();
        }
        Self {
            flows,
            reach,
            through,
        }
    }

    /// The flows that reach one site: those with a hop that ends at its
    /// Gap, or, for an observed use alone, those with a non-Gap hop at the
    /// using Symbol.
    fn of(&self, site: &Site<'_>) -> &[usize] {
        let found = match site.gap {
            Some(gap) => self.reach.get(gap.id.as_str()),
            None => site.caller().and_then(|caller| self.through.get(caller)),
        };
        found.map_or(&[], Vec::as_slice)
    }

    /// The entities with a `READS` or `WRITES` relation from a Symbol on a
    /// non-Gap hop of one flow, by entity id.
    fn entities<'m>(&self, index: usize, model: &Model<'m>) -> BTreeMap<&'m str, Access<'m>> {
        let mut entities: BTreeMap<&str, Access<'_>> = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for hop in &self.flows[index].hops {
            if hop.provenance.confidence_tier == ConfidenceTier::Gap {
                continue;
            }
            for end in [hop.src.as_str(), hop.dst.as_str()] {
                if !seen.insert(end) {
                    continue;
                }
                for edge in model.access.get(end).into_iter().flatten().copied() {
                    let entry = entities.entry(edge.dst.as_str()).or_default();
                    let slot = if edge.label == "WRITES" {
                        &mut entry.written
                    } else {
                        &mut entry.read
                    };
                    slot.get_or_insert(edge);
                }
            }
        }
        entities
    }
}

/// One outside target with the flows that reach it and their entities.
struct Reached<'m> {
    /// Flow indexes, each once, in trigger order.
    flows: Vec<(usize, BTreeMap<&'m str, Access<'m>>)>,
}

impl<'m> Reached<'m> {
    fn new(sites: &[Site<'_>], flows: &Flows<'_>, model: &Model<'m>) -> Self {
        let indexes: BTreeSet<usize> = sites
            .iter()
            .flat_map(|site| flows.of(site))
            .copied()
            .collect();
        Self {
            flows: indexes
                .into_iter()
                .map(|index| (index, flows.entities(index, model)))
                .collect(),
        }
    }

    /// Every entity of every flow that reaches the target.
    fn entities(&self) -> Vec<Use<'m>> {
        let mut all: BTreeMap<&str, (bool, bool)> = BTreeMap::new();
        for (_, entities) in &self.flows {
            for (id, access) in entities {
                let entry = all.entry(*id).or_default();
                entry.0 |= access.read.is_some();
                entry.1 |= access.written.is_some();
            }
        }
        all.into_iter().collect()
    }
}

/// One entity id with whether it is read and whether it is written.
type Use<'m> = (&'m str, (bool, bool));

/// The entities cell: each entity once, with how the flows use it.
struct Entities<'p, 'm>(&'p [Use<'m>], &'p Model<'m>);
impl fmt::Display for Entities<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut any = false;
        for (id, (read, written)) in self.0.iter().copied() {
            if any {
                f.write_str("; ")?;
            }
            any = true;
            match self.1.by_id.get(id) {
                Some(node) => write!(f, "{}", Literal(NodeName(node)))?,
                None => write!(f, "{}", Literal(id))?,
            }
            f.write_str(match (read, written) {
                (true, true) => " (read, written)",
                (false, true) => " (written)",
                _ => " (read)",
            })?;
        }
        if !any {
            f.write_str("none recovered")?;
        }
        Ok(())
    }
}

/// Tier, confidence and span cells of the fact a row rests on. The first
/// evidence span, and how many more the fact index lists.
struct Standing<'a>(Option<Fact<'a>>);
impl fmt::Display for Standing<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(fact) = self.0 else {
            return f.write_str("— | — | —");
        };
        let provenance = fact.provenance();
        write!(
            f,
            "{:?} | {:?} | ",
            provenance.tier, provenance.confidence_tier
        )?;
        let Some(first) = provenance.evidence.first() else {
            return f.write_str("—");
        };
        write!(
            f,
            "{}:{} bytes {}..{} @ {}",
            Literal(&first.repo),
            Literal(&first.path),
            first.byte_start,
            first.byte_end,
            Literal(&first.commit_sha)
        )?;
        match provenance.evidence.len() - 1 {
            0 => Ok(()),
            more => write!(f, " (+{more} more in the fact index)"),
        }
    }
}

/// The Symbol or file a relation starts from.
struct Caller<'a>(Option<&'a str>);
impl fmt::Display for Caller<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            None => f.write_str("not among the facts this export admits"),
            Some(src) if src.starts_with("file:") => f.write_str("file level"),
            // The symbol's name in its file; the whole id when the id does
            // not carry one. The fact index has the id either way.
            Some(src) => write!(
                f,
                "{}",
                Literal(src.rsplit_once('#').map_or(src, |(_, name)| name))
            ),
        }
    }
}

/// The repositories of the relations that reach a target, or of its fact.
struct Repositories<'p, 'a>(&'p Row<'a>);
impl fmt::Display for Repositories<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut repos: BTreeSet<&str> = self
            .0
            .edges
            .iter()
            .filter_map(|edge| places(&edge.props).next())
            .map(|(repo, _, _)| repo)
            .collect();
        if repos.is_empty() {
            repos.extend(places(&self.0.node.props).next().map(|(repo, _, _)| repo));
        }
        if repos.is_empty() {
            return f.write_str("—");
        }
        for (index, repo) in repos.into_iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{}", Literal(repo))?;
        }
        Ok(())
    }
}

/// The path of a URL: no scheme, host, query or fragment.
fn url_path(url: &str) -> &str {
    let url = url.split(['?', '#']).next().unwrap_or(url);
    match url.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("/", |at| &rest[at..]),
        None => url,
    }
}

/// What an unclassified target is: the call, the client or the channel.
struct Named<'p, 'a>(&'p Row<'a>);
impl fmt::Display for Named<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let props = &self.0.node.props;
        match self.0.kind {
            Kind::Call => {
                let request = &props["request"];
                let Some(url) = request["url"].as_str() else {
                    return f.write_str("URL not recovered");
                };
                write!(
                    f,
                    "{} {}{}",
                    Literal(request["method"].as_str().unwrap_or("?")),
                    Literal(origin(url).unwrap_or_default()),
                    Literal(url_path(url))
                )
            }
            Kind::Channel => write!(
                f,
                "{}: {}",
                Literal(props["kind"].as_str().unwrap_or("channel")),
                Literal(props["identity"].as_str().unwrap_or(&self.0.node.id))
            ),
            Kind::HttpClient | Kind::SdkClient | Kind::AuthProvider | Kind::Client => {
                match (
                    props["provider"].as_str(),
                    props["identity"].as_str(),
                    props["env_key"].as_str(),
                ) {
                    (Some(provider), Some(identity), _) => {
                        write!(f, "{}: {}", Literal(provider), Literal(identity))
                    }
                    (Some(provider), None, Some(key)) => write!(
                        f,
                        "{}: configuration key {}",
                        Literal(provider),
                        Literal(key)
                    ),
                    (Some(provider), None, None) => write!(f, "{}", Literal(provider)),
                    (None, ..) => write!(f, "{}", Literal(NodeName(self.0.node))),
                }
            }
        }
    }
}

fn note(weak: bool) -> &'static str {
    if weak {
        "rests on an InferredWeak fact; a verified-only export omits this row"
    } else {
        "—"
    }
}

fn weak<'a>(mut facts: impl Iterator<Item = Fact<'a>>) -> bool {
    facts.any(|fact| confidence(fact.props()) == ConfidenceTier::InferredWeak)
}

const INTRO: &str = "# Calls and publishes that leave the system boundary\n\n\
A deterministic projection of what the ingested sources call or publish to outside themselves: \
each outside HTTP origin with its call sites, the flows that reach it and the data entities \
those flows read or write. No model is involved, and no fact is added or changed.\n\n\
## The boundary and its limits\n\n\
- **Inside** is what the registered sources establish: a recovered endpoint of a registered \
repository, a host that a declared service or gateway binding of the system manifest covers, \
and a channel with a recovered subscriber or a backing resource.\n\
- **Outside** is claimed only from a literal: an outbound HTTP call, or an observed HTTP \
client, whose scheme and host are written in the source and that no declared binding covers. A \
service the registered sources do not contain is outside, including an unregistered sibling \
service.\n\
- **The outside list is a lower bound.** A call whose host is computed at run time or read \
from configuration, a client of a hosted provider and a channel nobody is seen to consume are \
listed as unclassified, never as outside and never as inside. Read the unclassified count \
beside the outside count.\n\
- **Without declared bindings, a call to another registered repository by absolute URL is \
listed as outside.** Only a service base URL or a gateway route in the system manifest places \
such a host inside.\n\
- **A declared binding is known here only through the graph.** The system manifest is not \
stored. Binding evidence records what it found on each call and observed HTTP client it \
evaluated: the binding that joined a call to an endpoint, the bindings that name a host, and \
for a call it kept, whether a declared binding names its host. A call kept by an ingest that \
did not yet record that outcome carries it as a sentence only: it is unclassified, with the \
sentence shown as recorded, until the system is ingested again.\n\
- **An absolute URL whose path matches an endpoint of the calling repository** is recorded as \
a call to that endpoint when no binding covers its host, and so is not listed here.\n\
- Origins are compared as scheme, host and port, with scheme and host in lower case. No DNS \
lookup, suffix list or allowlist is used.\n\
- Not covered: how sensitive the data is, what a request body contains, traffic that arrives \
from outside, network egress with no evidence in code or infrastructure, package dependencies, \
and who operates an SDK provider.\n\
- Every row carries the tier and confidence of the fact that holds its literal, never a higher \
one. An outbound call that matched no endpoint is a Gap fact, and its row says so. The fact \
index lists every span.\n\n";

const NOT_LISTED: &str = "\n## What is not listed\n\n\
- A call resolved to an endpoint of a registered repository, in the same repository or through \
a declared binding.\n\
- A call without a host: it goes to the caller's own origin. Those that matched no endpoint \
are counted above and listed in the gap register.\n\
- An in-process event channel, browser storage and an imported package.\n\
- A channel with no recovered publisher: it is inbound.\n\
- A flow that stops at an unresolved hop before the outbound call. It has no hop at the call, \
so it is not listed as reaching the target, and it stays partial in the gap register.\n\
- A channel is never listed as outside: the graph does not show who owns a queue or topic.\n";

/// The facts already cited, so each is cited once.
#[derive(Default)]
struct Cited<'a> {
    nodes: BTreeSet<&'a str>,
    edges: BTreeSet<(&'a str, &'a str, &'a str)>,
}

impl<'a> Cited<'a> {
    fn cite(&mut self, assertions: &mut Assertions, fact: Fact<'a>) -> Result<(), RenderLimit> {
        match fact {
            Fact::Node(node) if self.nodes.insert(&node.id) => assertions.node(node, None),
            Fact::Edge(edge) if self.edges.insert((&edge.src, &edge.label, &edge.dst)) => {
                assertions.edge(edge)
            }
            Fact::Node(_) | Fact::Edge(_) => Ok(()),
        }
    }
}

/// `external.md` over the facts and flows the export mode admitted and the
/// count of boundary facts it withheld.
pub(super) fn output(
    nodes: &[&Node],
    edges: &[&Edge],
    flows: &[Flow],
    (mode, withheld): (ExportMode, usize),
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let model = Model::new(nodes, edges);
    let flows = Flows::new(flows, &model);
    let reached: Vec<Reached<'_>> = model
        .outside
        .values()
        .map(|sites| Reached::new(sites, &flows, &model))
        .collect();

    // Each fact once, in the order the document lists it.
    let mut assertions = Assertions::new(&budget);
    let mut cited = Cited::default();
    for (sites, reached) in model.outside.values().zip(&reached) {
        for fact in sites.iter().flat_map(Site::facts) {
            cited.cite(&mut assertions, fact)?;
        }
        for (id, access) in reached.flows.iter().flat_map(|(_, entities)| entities) {
            if let Some(node) = model.by_id.get(*id) {
                cited.cite(&mut assertions, Fact::Node(node))?;
            }
            for edge in [access.read, access.written].into_iter().flatten() {
                cited.cite(&mut assertions, Fact::Edge(edge))?;
            }
        }
    }
    for fact in model.unclassified.iter().flat_map(Row::facts) {
        cited.cite(&mut assertions, fact)?;
    }

    let mut content = Text::new(&budget, INTRO)?;
    writeln!(
        content,
        "Export mode: `{}`. Outside targets: {}. Unclassified targets: {}. Calls that binding \
         evidence places in a registered repository and that matched no one endpoint there \
         (inside, unresolved): {}. Calls to the caller's own origin that matched no endpoint: \
         {}. Published channels with a recovered subscriber or backing resource: {}.\n",
        match mode {
            ExportMode::VerifiedOnly => "verified-only",
            ExportMode::BestEffort => "best-effort",
        },
        model.outside.len(),
        model.unclassified.len(),
        model.inside_unresolved,
        model.same_origin,
        model.inside_channels,
    )?;
    match mode {
        ExportMode::VerifiedOnly => writeln!(
            content,
            "Boundary facts left out of this export because they are InferredWeak or rejected: \
             {withheld}. A `best-effort` export includes the InferredWeak ones, marked. A fact \
             left out can remove a row, and a subscriber left out can list its channel as \
             unclassified.\n"
        )?,
        ExportMode::BestEffort => writeln!(
            content,
            "InferredWeak rows are included and marked in the Note column. Boundary facts left \
             out of this export because they are rejected: {withheld}.\n"
        )?,
    }

    content.push_str(
        "## External services\n\n\
         One row for each outside origin.\n\n\
         | Origin | Class | Sites | Flows that reach it | Entities read or written on these \
         flows | Note |\n\
         |---|---|---|---|---|---|\n",
    )?;
    for ((origin, sites), reached) in model.outside.iter().zip(&reached) {
        writeln!(
            content,
            "| {} | outside | {} | {} | {} | {} |",
            Literal(origin),
            sites.len(),
            reached.flows.len(),
            Entities(&reached.entities(), &model),
            note(sites.iter().any(|site| weak(site.facts())))
        )?;
    }
    if model.outside.is_empty() {
        content.push_str("| — | None in this export | 0 | 0 | — | — |\n")?;
    }

    content.push_str(
        "\n## Sites of each external service\n\n\
         One row for each call or use. A call that is both an outbound call and an observed \
         client use is one row. Tier and confidence are those of the fact that holds the \
         literal, the lowest where a row rests on more than one.\n\n\
         | Origin | Caller | Method | Path | Client | Tier | Confidence | Span | Flows through \
         this site | Note |\n\
         |---|---|---|---|---|---|---|---|---|---|\n",
    )?;
    for (origin, sites) in &model.outside {
        for site in sites {
            let request = site.gap.map(|gap| &gap.props["request"]);
            let text = |key: &str| {
                request
                    .and_then(|request| request[key].as_str())
                    .filter(|value| !value.is_empty())
            };
            write!(
                content,
                "| {} | {} | {} | {} | {} | {} | ",
                Literal(origin),
                Caller(site.caller()),
                Literal(text("method").unwrap_or("—")),
                // Only the base is in source: the path is built at run time (#2065).
                Literal(match request {
                    Some(request) if request["path_computed"] == true => "computed at run time",
                    _ => text("url").map_or("—", url_path),
                }),
                Literal(
                    site.uses
                        .and_then(|edge| edge.props["client"].as_str())
                        .unwrap_or("—")
                ),
                Standing(lowest(site.facts())),
            )?;
            let through = flows.of(site);
            for (position, index) in through.iter().enumerate() {
                if position > 0 {
                    content.push_str("; ")?;
                }
                write!(content, "{}", Literal(&flows.flows[*index].trigger_name))?;
            }
            if through.is_empty() {
                content.push_str("no flow recovered through this site")?;
            }
            writeln!(content, " | {} |", note(weak(site.facts())))?;
        }
    }
    if model.outside.is_empty() {
        content.push_str("| — | None in this export | — | — | — | — | — | — | — | — |\n")?;
    }

    content.push_str(
        "\n## External data flows\n\n\
         One row for each flow and outside origin it reaches. The entities are those a symbol \
         on a resolved hop of the flow reads or writes. The row does not say that an entity is \
         sent to the origin.\n\n\
         | Origin | Entry point | Entry kind | Flow status | Entities read or written on these \
         flows |\n\
         |---|---|---|---|---|\n",
    )?;
    let mut rows = 0usize;
    for (origin, reached) in model.outside.keys().zip(&reached) {
        for (index, entities) in &reached.flows {
            rows += 1;
            let flow = flows.flows[*index];
            let cells: Vec<Use<'_>> = entities
                .iter()
                .map(|(id, access)| (*id, (access.read.is_some(), access.written.is_some())))
                .collect();
            writeln!(
                content,
                "| {} | {} | {} | {} | {} |",
                Literal(origin),
                Literal(&flow.trigger_name),
                Literal(&flow.trigger_kind),
                flow.status.as_str(),
                Entities(&cells, &model),
            )?;
        }
    }
    if rows == 0 {
        content.push_str("| — | None in this export | — | — | — |\n")?;
    }

    content.push_str(
        "\n## Unclassified\n\n\
         One row for each target that leaves a registered repository's code and is not proven \
         inside or outside. The stop reason is the one recorded on the fact, unchanged.\n\n\
         | Target | Kind | Class | Repository | Reason | Stop reason recorded | Sites | Tier | \
         Confidence | Span | Note |\n\
         |---|---|---|---|---|---|---|---|---|---|---|\n",
    )?;
    for row in &model.unclassified {
        writeln!(
            content,
            "| {} | {} | unclassified | {} | {} | {} | {} | {} | {} |",
            Named(row),
            row.kind.label(),
            Repositories(row),
            row.reason.reason(),
            Literal(row.node.props["reason"].as_str().unwrap_or("—")),
            row.edges.len(),
            Standing(lowest(row.facts())),
            note(weak(row.facts()))
        )?;
    }
    if model.unclassified.is_empty() {
        content.push_str("| — | None in this export | — | — | — | — | 0 | — | — | — | — |\n")?;
    }

    content.push_str(NOT_LISTED)?;
    super::render::inline_literal(&mut content, &assertions.values)?;
    Ok(Output {
        content: content.finish(),
        assertions: assertions.values,
        subjects: assertions.subjects,
    })
}

/// Borrowed index roles of this producer, for the compile workspace model.
/// Rows hold references only; provenance is decoded one row at a time. An
/// origin is one owned string for each binding citation or outside target,
/// and a citation is a JSON value of its fact, so those are charged by
/// values. The flow indexes are charged to the flows the caller admitted.
pub(super) fn logical_index_bytes(rows: usize, values: usize) -> Option<usize> {
    use super::accounting::{map_slots, set_slots, sized_slots, slots, sum};
    sum(&[
        map_slots::<&str, &Node>(rows)?,                // by_id
        map_slots::<&str, Vec<&Edge>>(rows)?,           // relations by target
        slots::<&Edge>(rows)?,                          // their elements
        map_slots::<&str, usize>(rows)?,                // subscribers, backing
        set_slots::<Place<'static>>(rows)?,             // resolved call sites
        map_slots::<Place<'static>, &str>(rows)?,       // outbound calls by site
        map_slots::<&str, (&Edge, &Node)>(rows)?,       // observed use of a call
        slots::<Site<'static>>(rows)?,                  // one per outside site
        slots::<Row<'static>>(rows)?,                   // one per unclassified target
        slots::<&Edge>(rows)?,                          // their relations
        map_slots::<String, Vec<Site<'static>>>(rows)?, // outside targets
        set_slots::<String>(values)?,                   // cited origins
        sized_slots(values.checked_add(rows)?, 256)?,   // origin text
        map_slots::<&str, Access<'static>>(rows)?,      // entities of the flows
        set_slots::<&str>(rows)?,                       // cited nodes
        set_slots::<(&str, &str, &str)>(rows)?,         // cited relations
    ])
}

#[cfg(test)]
#[path = "external_tests.rs"]
mod tests;
