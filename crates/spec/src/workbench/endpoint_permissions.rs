//! Endpoints against permission sites (#1787, AC-0599, catalogue D6 §6.3):
//! `endpoint-permissions.md`.
//!
//! A deterministic (T0) join of two recovered fact sets: `Endpoint` facts and
//! the permission sites the adapters record on them. No model is called and
//! no permission site is recovered here.
//!
//! **Permission sites.** Two shapes exist today, both properties of the
//! `Endpoint` fact with their own provenance ([`RECOVERY`] lists the
//! producers): `permission_site` (NestJS GraphQL `Allow(...)` decorator
//! syntax) and `auth` (Flask and Flask-Admin guards, Spring method-security
//! annotations, Go route middleware, Rust auth extractors and axum layers). No adapter records a
//! site on a fact below the endpoint, so a check made inside a handler body
//! or in a function it calls is not recovered; the document says so.
//!
//! **Handler path.** From the endpoint: every `HANDLES` relation to its
//! handler, then `CALLS` relations in their stored direction, breadth first,
//! each fact visited once (so a cycle ends), at most
//! [`ENDPOINT_PATH_MAX_DEPTH`] calls below the handler, at most
//! [`ENDPOINT_PATH_MAX_SYMBOLS`] facts and [`ENDPOINT_PATH_MAX_RELATIONS`]
//! relations per endpoint. `DEPENDS_ON` is followed only into an execution
//! Gap, as the flow tracer does. A hop is unresolved when the relation or its
//! target is a Gap, or the target is not among the facts this export admits.
//! An endpoint with no recovered site whose path has an unresolved hop, no
//! handler or a reached bound is listed as partial, never under "none
//! recovered" (R-INT-4).
//!
//! **Cost.** One index over the admitted nodes and the three relation
//! labels, then per covered endpoint at most the bounds above: linear in the
//! graph plus endpoints times a constant.
//!
//! **Mode.** The producer receives the facts the export mode admitted, like
//! every other graph artifact. A permission site carries its own provenance
//! inside the endpoint fact, so it is admitted here by the same rule: a row
//! whose site the mode withholds is omitted and counted, before any
//! assertion is built (R-INT-5, ADR-0058).

use super::fact_index::citation_id;
use super::producers::Output;
use super::render::{Assertions, Budget, Literal, NodeName, RenderLimit, Text};
use super::{ExportMode, included, provenance};
use core_graph::FactKey;
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, Provenance};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::{self, Write as _};

/// Most `CALLS` relations followed below an endpoint's handler.
pub const ENDPOINT_PATH_MAX_DEPTH: usize = 16;
/// Most facts visited on one endpoint's handler path.
pub const ENDPOINT_PATH_MAX_SYMBOLS: usize = 256;
/// Most `CALLS` and `DEPENDS_ON` relations examined on one endpoint's
/// handler path.
pub const ENDPOINT_PATH_MAX_RELATIONS: usize = 2048;

/// Subject kind of a permission-site assertion.
const SITE_KIND: &str = "PermissionSite";

/// One framework whose endpoints carry a recovered permission site.
struct Recovery {
    /// The `framework` property of the endpoints it covers.
    framework: &'static str,
    language: &'static str,
    /// The endpoint property that holds the site.
    property: &'static str,
    recovers: &'static str,
    /// What the adapter read when it records no guard (`auth.state` none).
    absent_read: &'static str,
    producer: &'static str,
}

/// Every producer of a permission site, read from the adapters: the only
/// writers of `Endpoint.permission_site` and `Endpoint.auth`. An endpoint of
/// any other framework is outside recovery and is listed under the coverage
/// note, in neither table.
const RECOVERY: &[Recovery] = &[
    Recovery {
        framework: "flask",
        language: "Python",
        property: "auth",
        recovers: "guard decorators on the view function: flask_login login_required and \
                   fresh_login_required, flask_jwt_extended jwt_required() without arguments, and \
                   repository decorators whose wrapper returns or aborts with 401 or 403. Request \
                   hooks and wsgi_app middleware are read only to decide whether that reading is \
                   complete",
        absent_read: "view decorators, request hooks and wsgi_app middleware",
        producer: "adapters-lang-python (auth_guards)",
    },
    Recovery {
        framework: "flask_admin",
        language: "Python",
        property: "auth",
        recovers: "Flask-Admin view classes whose is_accessible returns flask_login \
                   current_user.is_authenticated",
        absent_read: "view decorators, request hooks and wsgi_app middleware",
        producer: "adapters-lang-python (flask_admin)",
    },
    Recovery {
        framework: "spring",
        language: "Java",
        property: "auth",
        recovers: "import-bound method-security annotations on the handler method, else its \
                   class (@PreAuthorize and @PostAuthorize SpEL role, authority and sign-in \
                   checks, @Secured, @RolesAllowed, @PermitAll and @DenyAll), and the first \
                   HttpSecurity authorizeHttpRequests rule whose matcher covers the endpoint, \
                   both with string constants the repository declares. Servlet filters and \
                   interceptors are not read",
        absent_read: "method-security annotations and HttpSecurity URL rules",
        producer: "adapters-lang-java (method_security)",
    },
    Recovery {
        framework: "net/http",
        language: "Go",
        property: "auth",
        recovers: GO_RECOVERS,
        absent_read: GO_ABSENT_READ,
        producer: GO_PRODUCER,
    },
    Recovery {
        framework: "chi",
        language: "Go",
        property: "auth",
        recovers: GO_RECOVERS,
        absent_read: GO_ABSENT_READ,
        producer: GO_PRODUCER,
    },
    Recovery {
        framework: "gin",
        language: "Go",
        property: "auth",
        recovers: GO_RECOVERS,
        absent_read: GO_ABSENT_READ,
        producer: GO_PRODUCER,
    },
    Recovery {
        framework: "axum",
        language: "Rust",
        property: "auth",
        recovers: "auth extractors on the handler (axum-extra TypedHeader<Authorization<Bearer>> \
                   or <Basic>, and repository FromRequestParts/FromRequest types that reject \
                   with 401 or 403) and layer/route_layer on the router after the route \
                   (tower-http ValidateRequestHeaderLayer and RequireAuthorizationLayer bearer \
                   or basic, and middleware::from_fn functions that reject with 401 or 403). \
                   Layers on an enclosing router are read through nest and merge",
        absent_read: "auth extractors and router layers",
        producer: RUST_PRODUCER,
    },
    Recovery {
        framework: "actix-web",
        language: "Rust",
        property: "auth",
        recovers: "auth extractors on the handler (actix-web-httpauth BearerAuth or BasicAuth, \
                   and repository FromRequest types that reject with 401 or 403). wrap() \
                   middleware is not read",
        absent_read: "auth extractors",
        producer: RUST_PRODUCER,
    },
    Recovery {
        framework: "nestjs-graphql",
        language: "TypeScript",
        property: "permission_site",
        recovers: "one Allow(...) decorator on a resolver operation, with Allow and Permission \
                   bound by named imports. The fact records decorator syntax, not what the \
                   decorator does at run time",
        absent_read: "resolver operation decorators",
        producer: "adapters-lang-ts (graphql_resolvers)",
    },
];

/// What the Go adapter reads for `net/http`, chi and gin (#2163). It never
/// records `none`: a Go route has no declarative public marker.
const GO_RECOVERS: &str = "middleware on the route: a handler wrapper, chi With, and Use on the \
                           same chi router or gin engine (gin: before the route). Guards are gin \
                           BasicAuth, chi BasicAuth and jwtauth Authenticator by import, and \
                           repository middleware whose body answers 401 or 403. Middleware on \
                           an enclosing router, a server-wide wrapper and gin groups are not read";
const GO_ABSENT_READ: &str = "route middleware";
const GO_PRODUCER: &str = "adapters-lang-go (middleware)";
/// The Rust adapter's producer (#2163). It never records `none`: a Rust
/// route has no declarative public marker.
const RUST_PRODUCER: &str = "adapters-lang-rust (auth)";

fn recovery(endpoint: &Node) -> Option<&'static Recovery> {
    let framework = endpoint.props["framework"].as_str()?;
    RECOVERY.iter().find(|r| r.framework == framework)
}

/// What the endpoint fact records about its permission site.
enum Reading<'a> {
    /// A recovered site with valid provenance.
    Site {
        description: String,
        provenance: Provenance,
    },
    /// The adapter read the endpoint and recorded no site.
    Absent { read: &'static str, basis: &'a str },
    /// The adapter could not finish its reading, or the site is unreadable.
    Incomplete(String),
}

fn site_provenance(raw: &serde_json::Value) -> Option<Provenance> {
    Provenance::deserialize(&*crate::stored_prov(raw))
        .ok()
        .filter(|provenance| provenance.validate().is_ok())
}

fn strings(value: &serde_json::Value) -> Vec<&str> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect()
}

const INVALID_SITE: &str = "a permission site is recorded without valid provenance";

fn reading<'a>(endpoint: &'a Node, recovery: &Recovery) -> Reading<'a> {
    let raw = &endpoint.props[recovery.property];
    if recovery.property == "permission_site" {
        if raw.is_null() {
            return Reading::Absent {
                read: "resolver operation decorators",
                basis: "no import-bound Allow decorator recovered",
            };
        }
        let arguments = strings(&raw["arguments"]);
        return match site_provenance(raw) {
            Some(provenance) if !arguments.is_empty() => Reading::Site {
                description: format!("Allow decorator: {}", arguments.join(", ")),
                provenance,
            },
            _ => Reading::Incomplete(INVALID_SITE.into()),
        };
    }
    match raw["state"].as_str() {
        Some("required") => {
            let guards = strings(&raw["guards"]);
            match site_provenance(raw) {
                Some(provenance) if !guards.is_empty() => Reading::Site {
                    description: format!(
                        "guard: {} (basis: {})",
                        guards
                            .iter()
                            .map(|g| guard_name(g))
                            .collect::<Vec<_>>()
                            .join(", "),
                        strings(&raw["basis"]).join(", ")
                    ),
                    provenance,
                },
                _ => Reading::Incomplete(INVALID_SITE.into()),
            }
        }
        Some("none") => Reading::Absent {
            read: recovery.absent_read,
            basis: raw["basis"].as_str().unwrap_or("no guard recorded"),
        },
        Some("unknown") => {
            const SHOWN: usize = 4;
            let reasons = strings(&raw["reasons"]);
            let mut text = String::from("the adapter could not classify the endpoint's guards");
            if !reasons.is_empty() {
                text.push_str(": ");
                text.push_str(&reasons[..reasons.len().min(SHOWN)].join(", "));
            }
            if reasons.len() > SHOWN {
                let _ = write!(text, ", and {} more", reasons.len() - SHOWN);
            }
            Reading::Incomplete(text)
        }
        _ => Reading::Incomplete("no guard reading is recorded for the endpoint".into()),
    }
}

/// The confidence a fact records, without validating its whole provenance.
/// A fact with none is unresolved, as [`provenance`] makes it.
fn confidence(props: &serde_json::Value) -> ConfidenceTier {
    ConfidenceTier::deserialize(&crate::stored_prov(props)["confidence_tier"])
        .unwrap_or(ConfidenceTier::Gap)
}

/// The admitted facts the walk reads, by reference.
struct Graph<'a> {
    by_id: BTreeMap<&'a str, &'a Node>,
    /// `HANDLES`, `CALLS` and `DEPENDS_ON` relations by source, in the
    /// caller's order (sorted by the export filter, and again here).
    out: BTreeMap<&'a str, Vec<&'a Edge>>,
}

impl<'a> Graph<'a> {
    fn new(nodes: &[&'a Node], edges: &[&'a Edge]) -> Self {
        let by_id = nodes.iter().map(|node| (node.id.as_str(), *node)).collect();
        let mut out: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
        for edge in edges
            .iter()
            .filter(|edge| matches!(edge.label.as_str(), "HANDLES" | "CALLS" | "DEPENDS_ON"))
        {
            out.entry(edge.src.as_str()).or_default().push(edge);
        }
        // Never the store's or the caller's order.
        for edges in out.values_mut() {
            edges.sort_by(|left, right| {
                (&left.label, &left.dst, &left.src).cmp(&(&right.label, &right.dst, &right.src))
            });
            edges.dedup_by(|left, right| {
                (&left.label, &left.dst, &left.src) == (&right.label, &right.dst, &right.src)
            });
        }
        Self { by_id, out }
    }

    fn out(&self, id: &str) -> &[&'a Edge] {
        self.out.get(id).map_or(&[], Vec::as_slice)
    }

    /// The flow tracer's execution-Gap rule (AC-0219): a `DEPENDS_ON` into a
    /// Gap that stands for code its owner runs, not a rule-evidence Gap.
    fn execution_gap(&self, edge: &Edge) -> bool {
        let node = self.by_id.get(edge.dst.as_str());
        let is_gap = node.map_or(edge.dst.starts_with("gap:"), |node| node.label == "Gap");
        let rule_scoped = |props: &serde_json::Value| props["rule_evidence_gap"] == true;
        is_gap && !rule_scoped(&edge.props) && !node.is_some_and(|node| rule_scoped(&node.props))
    }
}

/// What the walk of one endpoint's handler path established.
#[derive(Default)]
struct Path<'a> {
    /// The endpoint's `HANDLES` relations.
    handlers: Vec<&'a Edge>,
    /// Facts visited: handlers and what they call.
    symbols: usize,
    /// The first unresolved hop in walk order.
    first_unresolved: Option<&'a Edge>,
    /// Its recorded reason: the relation's, or else its Gap target's.
    reason: Option<&'a str>,
    unresolved: usize,
    /// A bound stopped the walk with relations left unread.
    bounded: bool,
    /// A fact on the path is InferredWeak (only in a `best-effort` export).
    weak: bool,
}

impl<'a> Path<'a> {
    fn partial(&self) -> bool {
        self.handlers.is_empty() || self.unresolved > 0 || self.bounded
    }

    fn unresolved(&mut self, graph: &Graph<'a>, edge: &'a Edge) {
        if self.first_unresolved.is_none() {
            self.first_unresolved = Some(edge);
            self.reason = edge.props["reason"].as_str().or_else(|| {
                let target: &'a Node = graph.by_id.get(edge.dst.as_str())?;
                target.props["reason"].as_str()
            });
        }
        self.unresolved += 1;
    }

    /// Read one relation's target: unresolved, already read, past a bound,
    /// or queued to have its own calls read.
    fn step(
        &mut self,
        graph: &Graph<'a>,
        frontier: &mut Frontier<'a>,
        edge: &'a Edge,
        depth: usize,
    ) {
        let tier = confidence(&edge.props);
        self.weak |= tier == ConfidenceTier::InferredWeak;
        let target = graph.by_id.get(edge.dst.as_str());
        let target_tier = target.map(|node| confidence(&node.props));
        if tier == ConfidenceTier::Gap
            || target.is_none_or(|node| node.label == "Gap")
            || target_tier == Some(ConfidenceTier::Gap)
        {
            self.unresolved(graph, edge);
        } else if frontier.visited.contains(edge.dst.as_str()) {
            // Already read: a cycle or a shared callee.
        } else if depth > ENDPOINT_PATH_MAX_DEPTH
            || frontier.visited.len() >= ENDPOINT_PATH_MAX_SYMBOLS
        {
            self.bounded = true;
        } else {
            self.weak |= target_tier == Some(ConfidenceTier::InferredWeak);
            frontier.visited.insert(edge.dst.as_str());
            frontier.queue.push_back((edge.dst.as_str(), depth));
        }
    }

    fn walk(graph: &Graph<'a>, endpoint: &Node) -> Self {
        let mut path = Self::default();
        let mut frontier = Frontier::default();
        for edge in graph.out(&endpoint.id) {
            if edge.label == "HANDLES" {
                path.handlers.push(edge);
                path.step(graph, &mut frontier, edge, 0);
            }
        }
        let mut examined = 0usize;
        'walk: while let Some((id, depth)) = frontier.queue.pop_front() {
            for edge in graph.out(id) {
                if edge.label == "HANDLES" {
                    continue;
                }
                if examined == ENDPOINT_PATH_MAX_RELATIONS {
                    path.bounded = true;
                    break 'walk;
                }
                examined += 1;
                if edge.label == "CALLS" {
                    path.step(graph, &mut frontier, edge, depth + 1);
                } else if graph.execution_gap(edge) {
                    path.weak |= confidence(&edge.props) == ConfidenceTier::InferredWeak;
                    path.unresolved(graph, edge);
                }
            }
        }
        path.symbols = frontier.visited.len();
        path
    }
}

/// The walk's working state for one endpoint.
#[derive(Default)]
struct Frontier<'a> {
    visited: BTreeSet<&'a str>,
    queue: VecDeque<(&'a str, usize)>,
}

struct PathText<'p, 'a>(&'p Path<'a>);
impl fmt::Display for PathText<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.0;
        if path.handlers.is_empty() {
            return f.write_str("partial: no handler is among the facts this export admits");
        }
        if !path.partial() {
            return write!(f, "read: {} facts, no unresolved hop", path.symbols);
        }
        write!(f, "partial: {} facts read", path.symbols)?;
        if let Some(edge) = path.first_unresolved {
            write!(
                f,
                "; unresolved hop {} {} {}",
                Literal(&edge.src),
                Literal(&edge.label),
                Literal(&edge.dst)
            )?;
            if let Some(reason) = path.reason {
                write!(f, " ({})", Literal(reason))?;
            }
            if path.unresolved > 1 {
                write!(f, ", and {} more", path.unresolved - 1)?;
            }
        }
        if path.bounded {
            write!(
                f,
                "; walk bound reached ({ENDPOINT_PATH_MAX_DEPTH} calls deep, \
                 {ENDPOINT_PATH_MAX_SYMBOLS} facts or {ENDPOINT_PATH_MAX_RELATIONS} relations)"
            )?;
        }
        Ok(())
    }
}

/// Where an endpoint is listed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    /// Table one: a recovered permission site.
    Sited,
    /// Table two: none recovered, and the handler path was read to its end.
    NoneRecovered,
    /// None recovered, and the reading or the handler path is incomplete.
    Partial,
    /// The coverage note: a framework with no permission-site recovery.
    Outside,
    /// Omitted: its permission site is withheld by the export mode or a
    /// rejection.
    Withheld,
}

/// One endpoint, by reference. Provenance is decoded when a row is written.
pub(super) struct Row<'a> {
    endpoint: &'a Node,
    recovery: Option<&'static Recovery>,
    path: Path<'a>,
    place: Place,
}

fn rows<'a>(
    nodes: &[&'a Node],
    edges: &[&'a Edge],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
) -> Vec<Row<'a>> {
    let graph = Graph::new(nodes, edges);
    let mut endpoints: Vec<&Node> = nodes
        .iter()
        .copied()
        .filter(|node| node.label == "Endpoint" && confidence(&node.props) != ConfidenceTier::Gap)
        .collect();
    endpoints.sort_by(|left, right| left.id.cmp(&right.id));
    endpoints.dedup_by(|left, right| left.id == right.id);
    endpoints
        .into_iter()
        .map(|endpoint| {
            let Some(recovery) = recovery(endpoint) else {
                return Row {
                    endpoint,
                    recovery: None,
                    path: Path::default(),
                    place: Place::Outside,
                };
            };
            let path = Path::walk(&graph, endpoint);
            let place = match reading(endpoint, recovery) {
                Reading::Site { provenance, .. } if included(&provenance, mode, rejected) => {
                    Place::Sited
                }
                Reading::Site { .. } => Place::Withheld,
                Reading::Absent { .. } if !path.partial() => Place::NoneRecovered,
                Reading::Absent { .. } | Reading::Incomplete(_) => Place::Partial,
            };
            Row {
                endpoint,
                recovery: Some(recovery),
                path,
                place,
            }
        })
        .collect()
}

/// A guard by its own name: a repository callable as its qualified name
/// (`require_api_auth`, `BaseAdminView.is_accessible`), never its graph id;
/// a library guard (`flask_login.login_required`) as recorded (#1979).
pub(super) fn guard_name(guard: &str) -> &str {
    match guard.strip_prefix("sym:") {
        Some(rest) => rest.rsplit_once('#').map_or(guard, |(_, name)| name),
        None => guard,
    }
}

/// Who a Python endpoint's recovered guard reading admits (#1979): the
/// caller a guard names, or any caller when the adapter read the endpoint
/// and recovered no guard. Decorator or hook syntax, never effective
/// runtime authorization.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Caller {
    /// The caller in a few words, and the guards that admit it by name.
    Guarded { role: String, guards: Vec<String> },
    /// The adapter read the endpoint and recovered no guard.
    Unguarded,
}

impl Caller {
    /// The caller as a story's subject: "a signed-in user".
    pub(super) fn subject(&self) -> String {
        match self {
            Self::Guarded { role, .. } => {
                let article =
                    if role.starts_with(['a', 'e', 'i', 'o', 'u', 'A', 'E', 'I', 'O', 'U']) {
                        "an"
                    } else {
                        "a"
                    };
                format!("{article} {role}")
            }
            Self::Unguarded => "any caller (no guard recovered)".into(),
        }
    }
}

/// The caller a guard admits, read from what the adapter recognized: a
/// Flask-Login or Flask-JWT-Extended decorator by its library name, a
/// Flask-Admin view by its `is_accessible`, and any other repository guard
/// as the guard itself; a Spring endpoint by the caller its annotation names.
/// The Python adapter records a Flask-Admin view as a
/// `flask` endpoint with the `flask_admin` mount.
fn role(endpoint: &Node, guards: &[&str]) -> String {
    // The Java adapter reads the caller from the annotation itself.
    if let Some(caller) = endpoint.props["auth"]["caller"].as_str() {
        return caller.to_owned();
    }
    let named = |guard: &str| match guard {
        "flask_login.login_required" => Some("signed-in user"),
        "flask_login.fresh_login_required" => Some("freshly signed-in user"),
        "flask_jwt_extended.jwt_required" | "jwtauth.Authenticator" => {
            Some("caller with a valid JWT")
        }
        "TypedHeader<Authorization<Bearer>>"
        | "actix_web_httpauth.BearerAuth"
        | "tower_http.ValidateRequestHeaderLayer::bearer"
        | "tower_http.RequireAuthorizationLayer::bearer" => Some("caller with a bearer token"),
        "gin.BasicAuth"
        | "gin.BasicAuthForRealm"
        | "chi.middleware.BasicAuth"
        | "TypedHeader<Authorization<Basic>>"
        | "actix_web_httpauth.BasicAuth"
        | "tower_http.ValidateRequestHeaderLayer::basic"
        | "tower_http.RequireAuthorizationLayer::basic" => {
            Some("caller with HTTP Basic credentials")
        }
        _ => None,
    };
    if endpoint.props["framework"] == "flask_admin" || endpoint.props["mount"] == "flask_admin" {
        return "administrator".to_owned();
    }
    let roles: Vec<String> = guards
        .iter()
        .map(|g| named(g).map_or_else(|| guard_role(guard_name(g)), str::to_owned))
        .collect();
    match &roles[..] {
        [first, rest @ ..] if rest.iter().all(|r| r == first) => first.clone(),
        _ => roles.join(" who is also "),
    }
}

/// The role a repository guard admits, read from the words of its name
/// (#1999): `require_api_auth` admits an API client, `validate_twilio_request`
/// a Twilio webhook. The guard stays the evidence; a name with no role word
/// reads as the check it is, in words, never as the identifier.
fn guard_role(guard: &str) -> String {
    let words: Vec<String> = guard
        .split(['.', '_'])
        .filter(|w| !w.is_empty())
        .flat_map(|w| {
            // `isAdmin` and `RequireApiKey` split at their capitals.
            let mut out = Vec::new();
            let mut word = String::new();
            for c in w.chars() {
                if c.is_uppercase() && !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
                word.extend(c.to_lowercase());
            }
            out.push(word);
            out
        })
        .collect();
    let has = |w: &str| words.iter().any(|x| x == w);
    let sudo = has("sudo") || has("fresh") || has("reauth");
    if has("admin") || has("superuser") || has("staff") {
        return "administrator".to_owned();
    }
    if has("api") || has("token") || has("key") {
        return if sudo {
            "API client in sudo mode".to_owned()
        } else {
            "API client".to_owned()
        };
    }
    if sudo {
        return "signed-in user in sudo mode".to_owned();
    }
    // `validate_twilio_request`, `verify_stripe_signature`: a third party
    // calling back.
    let check = ["validate", "verify", "check"];
    let proof = ["request", "signature", "webhook", "callback", "hmac"];
    if let [first, middle @ .., last] = &words[..]
        && check.contains(&first.as_str())
        && proof.contains(&last.as_str())
        && !middle.is_empty()
    {
        let vendor: Vec<String> = middle
            .iter()
            .map(|w| {
                let mut c = w.chars();
                c.next()
                    .map(|f| f.to_uppercase().chain(c).collect())
                    .unwrap_or_default()
            })
            .collect();
        return format!("{} webhook", vendor.join(" "));
    }
    if has("login") || has("auth") || has("authenticated") || has("user") || has("session") {
        return "signed-in user".to_owned();
    }
    format!("caller the “{}” check admits", words.join(" "))
}

/// The caller of `endpoint` as its guard reading states, when it is a
/// Python reading the export admits; `None` for any other endpoint and for
/// a reading that is withheld, unreadable or incomplete.
pub(super) fn caller(
    endpoint: &Node,
    mode: ExportMode,
    rejected: &BTreeSet<String>,
) -> Option<Caller> {
    caller_reading(endpoint, mode, rejected).map(|(caller, _)| caller)
}

/// The caller and the confidence of the reading that names it. An absent
/// reading with provenance passes the same admission as a guard: a rejected
/// or withheld "no guard" observation names no caller.
fn caller_reading(
    endpoint: &Node,
    mode: ExportMode,
    rejected: &BTreeSet<String>,
) -> Option<(Caller, Option<ConfidenceTier>)> {
    let recovery = recovery(endpoint).filter(|r| r.property == "auth")?;
    match reading(endpoint, recovery) {
        Reading::Site { provenance, .. } if included(&provenance, mode, rejected) => {
            let guards = strings(&endpoint.props["auth"]["guards"]);
            Some((
                Caller::Guarded {
                    role: role(endpoint, &guards),
                    guards: guards
                        .into_iter()
                        .map(|g| guard_name(g).to_owned())
                        .collect(),
                },
                Some(provenance.confidence_tier),
            ))
        }
        Reading::Absent { .. } => match site_provenance(&endpoint.props["auth"]) {
            Some(provenance) if !included(&provenance, mode, rejected) => None,
            provenance => Some((Caller::Unguarded, provenance.map(|p| p.confidence_tier))),
        },
        _ => None,
    }
}

/// D1 §1.2's frame of Python callers (#1979): one row per caller and guard
/// set, with how many operations it admits and a few of them, then the
/// operations with no guard recovered. Nothing is written when no endpoint
/// carries an admitted Python guard reading. Each operation a row counts is
/// asserted once, so its tier and citation are in the document's statements;
/// returns how many it asserted.
///
/// Each repository guard (a `sym:` id) of a counted reading is added to
/// `guards`, for what each guard checks (#1998, AC-0901).
pub(super) fn callers_frame(
    nodes: &[&Node],
    (mode, rejected): (ExportMode, &BTreeSet<String>),
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
    guards: &mut BTreeSet<String>,
    out: &mut Text,
) -> Result<usize, RenderLimit> {
    // Each operation by its printed name and citation id.
    let mut by_caller: BTreeMap<Caller, Vec<(String, String)>> = BTreeMap::new();
    let mut confidence: BTreeMap<Caller, BTreeSet<String>> = BTreeMap::new();
    let mut cited = 0;
    for endpoint in nodes.iter().filter(|n| n.label == "Endpoint") {
        if let Some((caller, tier)) = caller_reading(endpoint, mode, rejected) {
            if matches!(caller, Caller::Guarded { .. }) {
                guards.extend(
                    strings(&endpoint.props["auth"]["guards"])
                        .into_iter()
                        .filter(|g| g.starts_with("sym:"))
                        .map(str::to_owned),
                );
            }
            let id = citation_id(&FactKey::Node {
                id: endpoint.id.clone(),
            });
            if asserted.insert(id.clone()) {
                assertions.node(endpoint, None)?;
                cited += 1;
            }
            confidence
                .entry(caller.clone())
                .or_default()
                .insert(tier.map_or_else(|| "not recorded".to_owned(), |t| format!("{t:?}")));
            by_caller
                .entry(caller)
                .or_default()
                .push((NodeName(endpoint).to_string(), id));
        }
    }
    if by_caller.is_empty() {
        return Ok(0);
    }
    let guarded: usize = by_caller
        .iter()
        .filter(|(c, _)| **c != Caller::Unguarded)
        .map(|(_, ops)| ops.len())
        .sum();
    let unguarded = by_caller.get(&Caller::Unguarded).map_or(0, Vec::len);
    writeln!(
        out,
        "\nCallers by recovered guard: {} callers over {guarded} guarded operations; \
         {unguarded} operations with no guard recovered. Read from each operation's guard \
         decorators, Flask-Admin `is_accessible` or Spring method-security annotations \
         (decorator, annotation or hook syntax), not effective \
         runtime authorization or an identified user; `security.md` lists each operation's \
         caller under its capability.\n",
        by_caller.len() - usize::from(unguarded > 0)
    )?;
    // The examples' citations, so a drafted claim about a caller can cite
    // the operations it is read from (#1748).
    out.push_str(
        "| Caller | Guard | Reading confidence | Operations | Examples | Citation |\n\
         |---|---|---|---|---|---|\n",
    )?;
    let mut rows: Vec<(&Caller, &Vec<(String, String)>)> = by_caller.iter().collect();
    // Most operations first; any caller last.
    rows.sort_by_key(|(caller, ops)| (**caller == Caller::Unguarded, std::cmp::Reverse(ops.len())));
    for (caller, operations) in rows {
        let (who, guards) = match caller {
            Caller::Guarded { role, guards } => {
                let mut who = role.clone();
                if let Some(first) = who.get_mut(0..1) {
                    first.make_ascii_uppercase();
                }
                (who, guards.join(", "))
            }
            Caller::Unguarded => ("Any caller".to_owned(), "none recovered".to_owned()),
        };
        let mut operations = operations.clone();
        operations.sort();
        let shown: Vec<&str> = operations
            .iter()
            .take(3)
            .map(|(name, _)| name.as_str())
            .collect();
        let ids: Vec<&str> = operations
            .iter()
            .take(3)
            .map(|(_, id)| id.as_str())
            .collect();
        let more = operations.len() - shown.len();
        writeln!(
            out,
            "| {} | {} | {} | {} | {}{} | {} |",
            Literal(&who),
            Literal(&guards),
            confidence[caller]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", "),
            operations.len(),
            Literal(shown.join("; ")),
            if more > 0 {
                format!("; and {more} more")
            } else {
                String::new()
            },
            ids.join(", "),
        )?;
    }
    Ok(cited)
}

/// Who may call an endpoint, as its recovered permission site states, for
/// the per-capability table in `security.md` (AC-0853).
pub(super) enum Access {
    /// The guard or permission set the site states, in a few words.
    Guarded(String),
    /// The adapter read the endpoint and recovered no site; `partial` when
    /// its handler path was not read to the end.
    NoneRecovered { partial: bool },
    /// Neither: an unreadable or withheld site, or a framework outside
    /// permission-site recovery.
    NotEstablished(&'static str),
}

/// The handler-path index [`Access`] is read against, built once.
pub(super) struct AccessIndex<'a>(Graph<'a>);

impl<'a> AccessIndex<'a> {
    pub(super) fn new(nodes: &[&'a Node], edges: &[&'a Edge]) -> Self {
        Self(Graph::new(nodes, edges))
    }

    pub(super) fn access(
        &self,
        endpoint: &Node,
        mode: ExportMode,
        rejected: &BTreeSet<String>,
    ) -> Access {
        let Some(recovery) = recovery(endpoint) else {
            return Access::NotEstablished("outside permission-site recovery");
        };
        match reading(endpoint, recovery) {
            Reading::Site { provenance, .. } if !included(&provenance, mode, rejected) => {
                Access::NotEstablished("site withheld in this export mode")
            }
            Reading::Site { .. } => {
                let raw = &endpoint.props[recovery.property];
                Access::Guarded(if recovery.property == "permission_site" {
                    format!("any of {}", strings(&raw["arguments"]).join(", "))
                } else {
                    strings(&raw["guards"])
                        .into_iter()
                        .map(guard_name)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
            }
            Reading::Absent { .. } => Access::NoneRecovered {
                partial: Path::walk(&self.0, endpoint).partial(),
            },
            Reading::Incomplete(_) => Access::NotEstablished("site not readable"),
        }
    }
}

/// The first evidence span, and how many more the fact index lists.
struct Span<'a>(&'a Provenance);
impl fmt::Display for Span<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(first) = self.0.evidence.first() else {
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
        match self.0.evidence.len() - 1 {
            0 => Ok(()),
            more => write!(f, " (+{more} more in the fact index)"),
        }
    }
}

/// Tier, confidence and span cells of one fact.
struct Evidence<'a>(&'a Provenance);
impl fmt::Display for Evidence<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} | {:?} | {}",
            self.0.tier,
            self.0.confidence_tier,
            Span(self.0)
        )
    }
}

/// The R-INT-5 annotation of a row that rests on an InferredWeak fact.
fn note(weak: bool) -> &'static str {
    if weak {
        "rests on an InferredWeak fact; a verified-only export omits this row"
    } else {
        "—"
    }
}

fn prop<'a>(node: &'a Node, key: &str) -> &'a str {
    node.props[key].as_str().unwrap_or("not recorded")
}

const INTRO: &str = "# Endpoints and permission sites\n\n\
A deterministic join of two sets of recovered facts: each endpoint, and the permission sites \
recovered on its handler path. No model is involved and nothing here judges whether an endpoint \
is safe.\n\n\
## How to read this document\n\n\
- **None recovered means exactly that.** No permission site was recovered for the endpoint. It \
does not mean that no check runs. A check made by middleware, by a gateway in front of the \
service, by a framework hook the adapters do not read or by code inside the handler body is \
outside what is recovered. That is a limit of the tool's coverage, not a finding about the \
system.\n\
- **A permission site** is a place in the source where an adapter recovered a guard or a \
permission requirement for the endpoint. The frameworks covered are listed below. An endpoint \
of any other framework is under the coverage note and in neither table.\n\
- **Sites are recorded on the endpoint.** No adapter records one inside a handler body or in a \
function the handler calls.\n\
- **The handler path** is the endpoint's handler and the calls reachable from it among the \
recovered facts. When it has an unresolved hop, no handler or reaches the walk bound, an \
endpoint with no recovered site is listed as partial: its path was not read to the end.\n\
- Every row carries the tier, confidence and source span of the endpoint, and of the permission \
site where there is one. The fact index lists every span.\n\n";

/// `endpoint-permissions.md` over the facts the export mode admitted.
pub(super) fn output(
    nodes: &[&Node],
    edges: &[&Edge],
    (mode, rejected): (ExportMode, &BTreeSet<String>),
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let rows = rows(nodes, edges, mode, rejected);
    let mut assertions = Assertions::new(&budget);
    let mut cited: BTreeSet<(&str, &str, &str)> = BTreeSet::new();
    for row in rows.iter().filter(|row| row.place != Place::Withheld) {
        assertions.node(row.endpoint, None)?;
        if let Some(recovery) = row.recovery
            && let Reading::Site {
                description,
                provenance,
            } = reading(row.endpoint, recovery)
        {
            assertions.derived(
                &format_args!("permission-site:{}", row.endpoint.id),
                SITE_KIND,
                &format_args!(
                    "Permission site on {}: {description}",
                    NodeName(row.endpoint)
                ),
                provenance,
            )?;
        }
        for edge in row.path.handlers.iter().chain(&row.path.first_unresolved) {
            if cited.insert((edge.src.as_str(), edge.label.as_str(), edge.dst.as_str())) {
                assertions.edge(edge)?;
            }
        }
    }

    let count = |place| rows.iter().filter(|row| row.place == place).count();
    let mut content = Text::new(&budget, INTRO)?;
    writeln!(
        content,
        "Export mode: `{}`. Endpoints read: {}. With a recovered permission site: {}. None \
         recovered: {}. Handler path partial: {}. Outside permission-site recovery: {}.\n",
        match mode {
            ExportMode::VerifiedOnly => "verified-only",
            ExportMode::BestEffort => "best-effort",
        },
        rows.len(),
        count(Place::Sited),
        count(Place::NoneRecovered),
        count(Place::Partial),
        count(Place::Outside),
    )?;
    let withheld = count(Place::Withheld);
    if withheld > 0 {
        writeln!(
            content,
            "{withheld} endpoints are omitted from every table: the permission site recovered \
             for each is InferredWeak or rejected, which this export withholds.\n"
        )?;
    }

    content.push_str(
        "## Permission-site recovery covered today\n\n\
         | Framework | Language | What is recovered | Recorded as | Producer |\n|---|---|---|---|---|\n",
    )?;
    for recovery in RECOVERY {
        writeln!(
            content,
            "| `{}` | {} | {} | `Endpoint.{}` | {} |",
            recovery.framework,
            recovery.language,
            recovery.recovers,
            recovery.property,
            recovery.producer
        )?;
    }

    content.push_str(
        "\n## Endpoints with recovered permission sites\n\n\
         | Endpoint | Framework | Endpoint tier | Endpoint confidence | Endpoint span | \
         Permission site | Site tier | Site confidence | Site span | Handler path | Note |\n\
         |---|---|---|---|---|---|---|---|---|---|---|\n",
    )?;
    for row in rows.iter().filter(|row| row.place == Place::Sited) {
        let recovery = row.recovery.expect("a sited endpoint is covered");
        let Reading::Site {
            description,
            provenance: site,
        } = reading(row.endpoint, recovery)
        else {
            unreachable!("a sited endpoint has a site")
        };
        let endpoint = provenance(&row.endpoint.props, &row.endpoint.id);
        let weak = row.path.weak
            || [&endpoint, &site]
                .iter()
                .any(|p| p.confidence_tier == ConfidenceTier::InferredWeak);
        writeln!(
            content,
            "| {} | `{}` | {} | {} | {} | {} | {} |",
            Literal(NodeName(row.endpoint)),
            recovery.framework,
            Evidence(&endpoint),
            Literal(&description),
            Evidence(&site),
            PathText(&row.path),
            note(weak)
        )?;
    }
    if count(Place::Sited) == 0 {
        content.push_str("| — | — | — | — | — | None in this export | — | — | — | — | — |\n")?;
    }

    content.push_str(
        "\n## Endpoints with none recovered\n\n\
         No permission site was recovered for these endpoints, and each handler path was read \
         to its end. See \"How to read this document\" for what that does and does not say.\n\n\
         | Endpoint | Framework | Tier | Confidence | Span | What the adapter read | Recorded \
         basis | Handler path | Note |\n|---|---|---|---|---|---|---|---|---|\n",
    )?;
    for row in rows.iter().filter(|row| row.place == Place::NoneRecovered) {
        let recovery = row.recovery.expect("a read endpoint is covered");
        let Reading::Absent { read, basis } = reading(row.endpoint, recovery) else {
            unreachable!("none recovered is an absent reading")
        };
        let endpoint = provenance(&row.endpoint.props, &row.endpoint.id);
        let weak = row.path.weak || endpoint.confidence_tier == ConfidenceTier::InferredWeak;
        writeln!(
            content,
            "| {} | `{}` | {} | {read} | {} | {} | {} |",
            Literal(NodeName(row.endpoint)),
            recovery.framework,
            Evidence(&endpoint),
            Literal(basis),
            PathText(&row.path),
            note(weak)
        )?;
    }
    if count(Place::NoneRecovered) == 0 {
        content.push_str("| — | — | — | — | — | None in this export | — | — | — |\n")?;
    }

    content.push_str(
        "\n## Endpoints whose handler path is partial\n\n\
         No permission site was recovered for these endpoints, and the reading is incomplete: \
         the adapter could not finish reading the endpoint's guards, or the handler path has an \
         unresolved hop, has no handler or reached the walk bound. They are not listed under \
         \"none recovered\" because the whole path was not read.\n\n\
         | Endpoint | Framework | Tier | Confidence | Span | Guard reading | Handler path | \
         Note |\n|---|---|---|---|---|---|---|---|\n",
    )?;
    for row in rows.iter().filter(|row| row.place == Place::Partial) {
        let recovery = row.recovery.expect("a read endpoint is covered");
        let guards = match reading(row.endpoint, recovery) {
            Reading::Incomplete(reason) => format!("incomplete: {reason}"),
            _ => "complete: none recovered".into(),
        };
        let endpoint = provenance(&row.endpoint.props, &row.endpoint.id);
        let weak = row.path.weak || endpoint.confidence_tier == ConfidenceTier::InferredWeak;
        writeln!(
            content,
            "| {} | `{}` | {} | {} | {} | {} |",
            Literal(NodeName(row.endpoint)),
            recovery.framework,
            Evidence(&endpoint),
            Literal(&guards),
            PathText(&row.path),
            note(weak)
        )?;
    }
    if count(Place::Partial) == 0 {
        content.push_str("| — | — | — | — | — | None in this export | — | — |\n")?;
    }

    content.push_str(
        "\n## Coverage note: endpoints outside permission-site recovery\n\n\
         No adapter recovers permission sites for the frameworks below, so these endpoints are \
         in neither table above. Nothing is stated about their checks.\n\n\
         | Language | Framework | Endpoints |\n|---|---|---|\n",
    )?;
    let outside: Vec<&Row<'_>> = rows
        .iter()
        .filter(|row| row.place == Place::Outside)
        .collect();
    let mut groups: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for row in &outside {
        *groups
            .entry((
                prop(row.endpoint, "language"),
                prop(row.endpoint, "framework"),
            ))
            .or_default() += 1;
    }
    for ((language, framework), endpoints) in &groups {
        writeln!(
            content,
            "| {} | {} | {endpoints} |",
            Literal(language),
            Literal(framework)
        )?;
    }
    if groups.is_empty() {
        content.push_str("| — | None in this export | 0 |\n")?;
    }
    content.push_str(
        "\n| Endpoint | Language | Framework | Tier | Confidence | Span | Note |\n\
         |---|---|---|---|---|---|---|\n",
    )?;
    for row in &outside {
        let endpoint = provenance(&row.endpoint.props, &row.endpoint.id);
        writeln!(
            content,
            "| {} | {} | {} | {} | {} |",
            Literal(NodeName(row.endpoint)),
            Literal(prop(row.endpoint, "language")),
            Literal(prop(row.endpoint, "framework")),
            Evidence(&endpoint),
            note(endpoint.confidence_tier == ConfidenceTier::InferredWeak)
        )?;
    }
    if outside.is_empty() {
        content.push_str("| — | — | None in this export | — | — | — | — |\n")?;
    }
    super::render::inline_literal(&mut content, &assertions.values)?;
    Ok(Output {
        content: content.finish(),
        assertions: assertions.values,
        subjects: assertions.subjects,
        attested: Vec::new(),
    })
}

/// Borrowed index roles of this producer, for the compile workspace model.
/// Rows hold references only; provenance is decoded one row at a time.
pub(super) fn logical_index_bytes(rows: usize) -> Option<usize> {
    use super::accounting::{map_slots, set_slots, slots, sum};
    sum(&[
        map_slots::<&str, &Node>(rows)?,         // by_id
        map_slots::<&str, Vec<&Edge>>(rows)?,    // relations by source
        slots::<&Edge>(rows)?,                   // their elements
        slots::<&Node>(rows)?,                   // endpoints
        slots::<Row<'static>>(rows)?,            // one per endpoint
        slots::<&Edge>(rows)?,                   // HANDLES relations across rows
        set_slots::<&str>(rows)?,                // visited, one endpoint at a time
        slots::<(&str, usize)>(rows)?,           // walk queue
        set_slots::<(&str, &str, &str)>(rows)?,  // cited relations
        slots::<&Row<'static>>(rows)?,           // coverage-note rows
        map_slots::<(&str, &str), usize>(rows)?, // coverage-note groups
    ])
}

#[cfg(test)]
#[path = "endpoint_permissions_tests.rs"]
mod tests;
