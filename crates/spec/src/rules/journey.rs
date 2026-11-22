//! Path-router journey candidate recovery under rule `journey-candidate@2`
//! (US-0045, AC-0352, ADR-0050).
//!
//! This module is pure: it reads borrowed graph slices (`&[&Node]`, `&[&Edge]`)
//! and traced server flows (`&[Flow]`), performs no I/O or store access, and
//! emits deterministic T2/InferredWeak [`JourneyCandidate`] values.

use crate::provenance;
use crate::workbench::diagrams::TestReferences;
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, ProducingMethod, Provenance, Tier, content_hash};
use flowtracer::Flow;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Named T2 rule producing journey candidates from path-router screens.
pub const JOURNEY_RULE: &str = "journey-candidate";

/// Rule version for `journey-candidate@2` (ADR-0084).
pub const JOURNEY_RULE_VERSION: &str = "2";

/// Recorded limitation for [`AssumptionKind::EntryCallback`].
pub const ENTRY_CALLBACK_LIMITATION: &str = "invocation not proven";

/// Recorded limitation for [`AssumptionKind::Startup`].
pub const STARTUP_LIMITATION: &str =
    "startup navigation runs at owner construction; order after the initial screen not proven";

/// Recorded limitation for [`AssumptionKind::HiltDispatch`].
pub const HILT_DISPATCH_LIMITATION: &str =
    "implementation bound by a Hilt @Binds; the runtime component and overrides not proven";

const EXTRACTOR: &str = "spec.journey";

/// Search bounds for `journey-candidate@2`.
///
/// The default budget values are part of the `journey-candidate@2` contract
/// (ADR-0084):
/// - `max_screens = 6`
/// - `max_expanded_states = 4096` (256 under `@1`)
/// - `max_calls_per_screen = 64`
/// - `max_candidates = 64` (32 under `@1`)
/// - `max_alternatives = 64`
///
/// The active budget is hashed into every candidate's `id` digest input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JourneyBudget {
    /// Maximum number of screens on a simple path (before any appended result step).
    pub max_screens: usize,
    /// Maximum number of expanded BFS path states per entry screen.
    pub max_expanded_states: usize,
    /// Maximum number of visited `Symbol` nodes in a screen's rendered-code call closure.
    pub max_calls_per_screen: usize,
    /// Maximum number of retained candidates per entry screen.
    pub max_candidates: usize,
    /// Maximum number of retained alternative candidates per entry screen
    /// (ADR-0084): paths through screens their goal's shortest path skips.
    pub max_alternatives: usize,
}

impl Default for JourneyBudget {
    fn default() -> Self {
        Self {
            max_screens: 6,
            max_expanded_states: 4096,
            max_calls_per_screen: 64,
            max_candidates: 64,
            max_alternatives: 64,
        }
    }
}

/// Basis under which a client call's stitched endpoint qualifies as a journey goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalBasis {
    /// A server flow triggered by the endpoint reaches a `WRITES` edge to a `DataEntity`.
    ObservedWrite,
    /// The HTTP method is `POST`, `PUT`, `PATCH`, or `DELETE`, with no observed write.
    MethodAssumed,
}

impl GoalBasis {
    /// Stable serialized string representation (`"observed_write"` or `"method_assumed"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ObservedWrite => "observed_write",
            Self::MethodAssumed => "method_assumed",
        }
    }
}

/// Whether a post-goal navigation was proven to execute after the goal call.
///
/// A navigation under a branch is [`JourneyResult::Ordered`] only when the
/// branch tests the goal call's own bound result (AC-1309).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JourneyResult {
    /// A navigation site in the same `Symbol` and file path at or after the
    /// `byte_end` of the fetch: unconditional after a fetch that is
    /// straight-line in that Symbol's body, or under a branch on the fetch's
    /// bound result.
    Ordered,
    /// No post-goal navigation with proven ordering.
    Unordered,
}

impl JourneyResult {
    /// Stable serialized string representation (`"ordered"` or `"unordered"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ordered => "ordered",
            Self::Unordered => "unordered",
        }
    }
}

/// Reason a candidate or its search was truncated by [`JourneyBudget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TruncationReason {
    /// Visited `Symbol` nodes in a screen's rendered-code closure hit `max_calls_per_screen`.
    CallsPerScreen,
    /// Expanded BFS path states for the entry hit `max_expanded_states`.
    ExpandedStates,
    /// Retained candidates for the entry hit `max_candidates`.
    Candidates,
    /// Retained alternative candidates for the entry hit `max_alternatives`.
    Alternatives,
    /// A path at `max_screens` still had an unvisited outgoing navigation.
    MaxScreens,
}

impl TruncationReason {
    /// Stable serialized string representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CallsPerScreen => "calls_per_screen",
            Self::ExpandedStates => "expanded_states",
            Self::Candidates => "candidates",
            Self::Alternatives => "alternatives",
            Self::MaxScreens => "max_screens",
        }
    }
}

/// One static navigation call site recorded on a `NAVIGATES_TO` edge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NavigationSite {
    /// Source-relative file path.
    pub path: String,
    /// Inclusive start byte.
    pub byte_start: u64,
    /// Exclusive end byte.
    pub byte_end: u64,
    /// Whether the navigation call site lies inside a branch or catch block.
    pub conditional: bool,
    /// The `(byte_start, byte_end)` of each call whose bound result a branch
    /// around this site tests (`const res = await call(…); if (res.status …)`),
    /// as the adapter recorded them; empty otherwise (AC-1309).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub result_of: Vec<(u64, u64)>,
}

/// The `NAVIGATES_TO` edge and sites taken to reach a step's screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepNavigation {
    /// Source `Symbol` id in the previous screen's closure.
    pub source: String,
    /// Target `Screen` id.
    pub target: String,
    /// Call sites taken on the `NAVIGATES_TO` edge.
    pub sites_taken: Vec<NavigationSite>,
}

/// One stitched client call (`FETCHES` -> `Endpoint`) in a screen's closure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepCall {
    /// Target `Endpoint` node id.
    pub endpoint: String,
    /// HTTP method (uppercase).
    pub method: String,
    /// Source `Symbol` id in the screen's closure.
    pub source: String,
    /// Fetch site evidence from `edge.props.prov.evidence[0]`.
    pub evidence: Option<EvidenceRef>,
    /// The relation's `straight_line`: whether every pass through `source`'s
    /// body that gets past the call evaluates it. `None` when the fact does
    /// not record it. Read for the result-step proof only; not serialized,
    /// because the relation is among the candidate's support facts.
    #[serde(skip)]
    pub straight_line: Option<bool>,
}

/// Kind of unresolved edge (`Gap` target) reached from a screen's closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedKind {
    /// Unresolved client call (`FETCHES` -> `Gap`).
    Call,
    /// Unresolved navigation (`NAVIGATES_TO` -> `Gap`).
    Navigation,
}

impl UnresolvedKind {
    /// Stable serialized string representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Navigation => "navigation",
        }
    }
}

/// An unresolved call or navigation (`Gap` target) recorded on a step.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct UnresolvedStepItem {
    /// Whether the unresolved edge is a `FETCHES` call or `NAVIGATES_TO` navigation.
    pub kind: UnresolvedKind,
    /// Source `Symbol` id in the screen's closure.
    pub source: String,
    /// Target `Gap` node id.
    pub gap_id: String,
}

/// Kind of assumption crossed by a journey candidate (`entry_callback`,
/// `startup` or `hilt_dispatch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssumptionKind {
    /// Callback passed inside `entry<X>` crossed as an assumed reachability step.
    EntryCallback,
    /// Init-phase back-stack navigation on an initial screen's owner class.
    Startup,
    /// Interface member crossed to the implementation a Hilt `@Binds` binds.
    HiltDispatch,
}

impl AssumptionKind {
    /// Stable serialized string representation (`"entry_callback"`,
    /// `"startup"` or `"hilt_dispatch"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EntryCallback => "entry_callback",
            Self::Startup => "startup",
            Self::HiltDispatch => "hilt_dispatch",
        }
    }
}

/// One direct parameter-forwarding hop along an `entry_callbacks` chain.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ForwardHop {
    /// Nested composable receiving the forwarded callback parameter.
    pub composable: String,
    /// Parameter name on the nested composable.
    pub param: String,
}

/// An `entry_callbacks` parameter on a screen that is never read on its forwarding chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnreadCallback {
    /// Parameter name on the entry's rendered composable.
    pub param: String,
    /// Entry's rendered composable name or symbol.
    pub composable: String,
    /// Target callback `Symbol` id passed at registration.
    pub symbol: String,
    /// Registration site evidence.
    pub registration: EvidenceRef,
}

/// A recorded reachability assumption (`entry_callback`, `startup` or
/// `hilt_dispatch`) on a [`JourneyCandidate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JourneyAssumption {
    /// Whether this assumption crosses an `entry_callback` or a `startup` navigation.
    pub kind: AssumptionKind,
    /// Step index in `steps` whose incoming navigation, calls, or unresolved items rely on it.
    pub step: usize,
    /// The `Screen` node id whose closure was extended by the assumption.
    pub screen: String,
    /// Callback `Symbol` id (`entry_callback`), back-stack owner class
    /// `Symbol` id (`startup`) or bound implementation member (`hilt_dispatch`).
    pub symbol: String,
    /// Interface member dispatched from (`hilt_dispatch` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
    /// Callback parameter name (`entry_callback` only).
    pub param: Option<String>,
    /// Entry composable receiving the callback (`entry_callback` only).
    pub composable: Option<String>,
    /// Registration site evidence: the `entry<X>` registration
    /// (`entry_callback`) or the `@Binds` declaration (`hilt_dispatch`).
    pub registration: Option<EvidenceRef>,
    /// Direct parameter-forwarding hops in order (`entry_callback` only).
    pub forwarded: Vec<ForwardHop>,
    /// Explicit limitation statement for the assumption.
    pub limitation: String,
}

/// One step (screen, incoming navigation, client calls, and unresolved edges) on a journey candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JourneyStep {
    /// `Screen` node id.
    pub screen: String,
    /// Navigation edge taken to enter this screen (`None` for the entry screen).
    pub navigation: Option<StepNavigation>,
    /// Client calls (`FETCHES` -> `Endpoint`) made from this screen's closure.
    pub calls: Vec<StepCall>,
    /// Unresolved calls and navigations (`Gap` targets) from this screen's closure.
    pub unresolved: Vec<UnresolvedStepItem>,
    /// Unread `entry_callbacks` registrations (`read == false`) on this screen, in record order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unread_callbacks: Vec<UnreadCallback>,
}

/// A recovered server-side data write (`Symbol` -> `DataEntity`) reached by a goal endpoint's flow.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WriteFact {
    /// Writing `Symbol` node id.
    pub symbol: String,
    /// Written `DataEntity` node id.
    pub data_entity: String,
}

/// An earlier `method_assumed` call encountered before an `observed_write` goal on a path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AssumedGoal {
    /// Screen id where the assumed goal call occurred.
    pub screen: String,
    /// Stitched `Endpoint` node id.
    pub endpoint: String,
    /// HTTP method (uppercase).
    pub method: String,
    /// Source `Symbol` id making the call.
    pub source: String,
    /// Write coverage status (`"unknown"`).
    pub write_coverage: String,
}

/// The goal endpoint and basis of a [`JourneyCandidate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JourneyGoal {
    /// Goal `Endpoint` node id.
    pub endpoint: String,
    /// HTTP method (uppercase).
    pub method: String,
    /// Precedence basis (`observed_write` or `method_assumed`).
    pub goal_basis: GoalBasis,
    /// `"unknown"` when `goal_basis == GoalBasis::MethodAssumed`, `None` when a write is observed.
    pub write_coverage: Option<String>,
    /// Recovered write facts (`Symbol` -> `DataEntity`) reached by the endpoint's server flows.
    pub writes: Vec<WriteFact>,
}

/// One bounded, rule-derived T2 journey candidate (`journey-candidate@2`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JourneyCandidate {
    /// Stable path identity: `journey:{repo}@journey-candidate:{digest}`.
    pub id: String,
    /// Content hash over the supporting facts, assumptions, and truncation state.
    pub support_fingerprint: String,
    /// Entry `Screen` node id.
    pub entry: String,
    /// Ordered steps along the journey (including `result_step` at the end when `result == Ordered`).
    pub steps: Vec<JourneyStep>,
    /// Goal endpoint and basis.
    pub goal: JourneyGoal,
    /// Earlier `method_assumed` calls encountered along the path before an `observed_write` goal.
    pub earlier_assumed_goals: Vec<AssumedGoal>,
    /// Whether a post-goal result screen navigation was proven ordered after the goal call.
    pub result: JourneyResult,
    /// Appended result step when `result == JourneyResult::Ordered`.
    pub result_step: Option<JourneyStep>,
    /// Trigger ids of attached server flows for the goal endpoint.
    pub flows: Vec<String>,
    /// Referenced `TestCase` node ids for the goal endpoint and its flow hops.
    pub tests: Vec<String>,
    /// Step indexes in `steps` whose incoming navigation edge is conditional.
    pub conditional_steps: Vec<usize>,
    /// Budget truncation reason, if any bound was hit.
    pub truncated: Option<TruncationReason>,
    /// Number of alternative simple paths found within the budget for the same `(entry, goal)`.
    pub alternatives: usize,
    /// False when a budget bound stopped the search before all simple paths were explored.
    pub alternatives_complete: bool,
    /// For an alternative candidate (ADR-0084), the `id` of the shortest
    /// candidate for the same `(entry, goal)`; `None` for that candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternative_of: Option<String>,
    /// Fact-derived display label, e.g. `Main → POST /api/v2/alias/custom/new → NewAliasResult`.
    pub label: String,
    /// Recorded reachability assumptions (`entry_callback` and `startup`), sorted and deduplicated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<JourneyAssumption>,
    /// T2/InferredWeak provenance with `ProducingMethod::Rule { rule: "journey-candidate", version: "1" }`.
    pub provenance: Provenance,
}

/// Per-entry search diagnostics for `journey-candidate@2`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntrySearchSummary {
    /// Entry `Screen` node id.
    pub entry: String,
    /// Number of BFS states expanded for this entry.
    pub expanded_states: usize,
    /// Number of candidates retained for this entry.
    pub candidates: usize,
    /// First budget bound hit during search for this entry, if any.
    pub truncated: Option<TruncationReason>,
    /// Outgoing navigations with an assumption across expanded screens, counted once per `(screen, source, target)`.
    pub assumed_navigations: usize,
    /// Unread callback records on expanded screens, counted once per record.
    pub unread_callbacks: usize,
}

/// Output of [`search`]: recovered [`JourneyCandidate`] values and per-entry search summaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JourneySearch {
    /// Recovered journey candidates in deterministic order.
    pub candidates: Vec<JourneyCandidate>,
    /// Per-entry search summaries in entry `(repo, id)` order.
    pub entries: Vec<EntrySearchSummary>,
}

fn hash_bytes(value: &impl Serialize) -> Vec<u8> {
    serde_json::to_vec(value).expect("journey candidate value serializes")
}

fn is_gap_node(node: &Node) -> bool {
    node.label == "Gap" || provenance(&node.props, &node.id).confidence_tier == ConfidenceTier::Gap
}

fn edge_confidence(edge: &Edge) -> ConfidenceTier {
    provenance(
        &edge.props,
        &format!("{} {} {}", edge.src, edge.label, edge.dst),
    )
    .confidence_tier
}

/// A Kotlin `@Binds` hop from an interface member to its bound
/// implementation (W11c-3): crossed only as a `hilt_dispatch` assumption.
fn is_dispatch_edge(edge: &Edge) -> bool {
    edge.label == "CALLS" && edge.props["resolution"].as_str() == Some("hilt-binds")
}

/// A proven call: crossed by every closure without an assumption. A
/// function's rendered template and a template's `extends` / `include`
/// count as calls (AC-0855).
fn is_proven_call(edge: &Edge, by_id: &BTreeMap<&str, &Node>) -> bool {
    matches!(
        edge.label.as_str(),
        "CALLS" | "RENDERS_TEMPLATE" | "INCLUDES"
    ) && !is_dispatch_edge(edge)
        && !is_gap_edge(edge, by_id)
}

/// The `hilt_dispatch` assumption a dispatch edge records, citing the
/// `@Binds` declaration its provenance names.
fn dispatch_assumption(edge: &Edge) -> Option<NavAssumption> {
    let binding = provenance(
        &edge.props,
        &format!("{} {} {}", edge.src, edge.label, edge.dst),
    )
    .evidence
    .into_iter()
    .next()?;
    Some(NavAssumption::HiltDispatch {
        interface: edge.src.clone(),
        implementation: edge.dst.clone(),
        binding,
    })
}

fn is_gap_edge(edge: &Edge, by_id: &BTreeMap<&str, &Node>) -> bool {
    edge_confidence(edge) == ConfidenceTier::Gap
        || by_id.get(edge.dst.as_str()).is_some_and(|n| is_gap_node(n))
}

fn screen_repo(screen_id: &str) -> &str {
    screen_id
        .strip_prefix("screen:")
        .and_then(|rest| rest.split_once('@'))
        .map_or("", |(repo, _)| repo)
}

fn node_repo(node: &Node) -> &str {
    if let Some(repo) = node.props["repo"].as_str().filter(|s| !s.is_empty()) {
        return repo;
    }
    node.id
        .split_once(':')
        .and_then(|(_, rest)| rest.split_once('@'))
        .map_or("", |(repo, _)| repo)
}

fn same_repo(screen_repo: &str, symbol: &Node) -> bool {
    let sym_repo = node_repo(symbol);
    screen_repo == sym_repo
}

fn parse_navigation_sites(edge: &Edge) -> Vec<NavigationSite> {
    let edge_cond = edge.props["conditional"].as_bool().unwrap_or(false);
    if let Some(sites) = edge.props["sites"].as_array() {
        let parsed: Vec<NavigationSite> = sites
            .iter()
            .filter_map(|site| {
                Some(NavigationSite {
                    path: site["path"].as_str()?.to_owned(),
                    byte_start: site["byte_start"].as_u64()?,
                    byte_end: site["byte_end"].as_u64()?,
                    conditional: site["conditional"].as_bool().unwrap_or(edge_cond),
                    result_of: result_of(site),
                })
            })
            .collect();
        if !parsed.is_empty() {
            return parsed;
        }
    }
    let prov = provenance(
        &edge.props,
        &format!("{} {} {}", edge.src, edge.label, edge.dst),
    );
    prov.evidence
        .into_iter()
        .map(|ev| NavigationSite {
            path: ev.path,
            byte_start: ev.byte_start,
            byte_end: ev.byte_end,
            conditional: edge_cond,
            result_of: Vec::new(),
        })
        .collect()
}

fn result_of(site: &serde_json::Value) -> Vec<(u64, u64)> {
    site["result_of"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|range| Some((range["byte_start"].as_u64()?, range["byte_end"].as_u64()?)))
        .collect()
}

fn parse_init_navigation_sites(edge: &Edge) -> Vec<NavigationSite> {
    let edge_cond = edge.props["conditional"].as_bool().unwrap_or(false);
    let Some(sites) = edge.props["sites"].as_array() else {
        return Vec::new();
    };
    sites
        .iter()
        .filter(|site| site["phase"].as_str() == Some("init"))
        .filter_map(|site| {
            Some(NavigationSite {
                path: site["path"].as_str()?.to_owned(),
                byte_start: site["byte_start"].as_u64()?,
                byte_end: site["byte_end"].as_u64()?,
                conditional: site["conditional"].as_bool().unwrap_or(edge_cond),
                result_of: result_of(site),
            })
        })
        .collect()
}

fn parse_callback_evidence(
    raw: &serde_json::Value,
    default_repo: &str,
    default_sha: &str,
) -> Option<EvidenceRef> {
    Some(EvidenceRef {
        repo: raw["repo"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(default_repo)
            .to_owned(),
        path: raw["path"].as_str()?.to_owned(),
        byte_start: raw["byte_start"].as_u64()?,
        byte_end: raw["byte_end"].as_u64()?,
        commit_sha: raw["commit_sha"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(default_sha)
            .to_owned(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EntryCallbackRecord {
    param: String,
    symbol: String,
    composable: String,
    registration: EvidenceRef,
    forwarded: Vec<ForwardHop>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NavAssumption {
    EntryCallback(EntryCallbackRecord),
    Startup {
        owner: String,
    },
    HiltDispatch {
        interface: String,
        implementation: String,
        binding: EvidenceRef,
    },
}

impl NavAssumption {
    fn to_journey_assumption(&self, step: usize, screen: &str) -> JourneyAssumption {
        match self {
            Self::EntryCallback(rec) => JourneyAssumption {
                kind: AssumptionKind::EntryCallback,
                step,
                screen: screen.to_owned(),
                symbol: rec.symbol.clone(),
                interface: None,
                param: Some(rec.param.clone()),
                composable: Some(rec.composable.clone()),
                registration: Some(rec.registration.clone()),
                forwarded: rec.forwarded.clone(),
                limitation: ENTRY_CALLBACK_LIMITATION.to_owned(),
            },
            Self::Startup { owner } => JourneyAssumption {
                kind: AssumptionKind::Startup,
                step,
                screen: screen.to_owned(),
                symbol: owner.clone(),
                interface: None,
                param: None,
                composable: None,
                registration: None,
                forwarded: Vec::new(),
                limitation: STARTUP_LIMITATION.to_owned(),
            },
            Self::HiltDispatch {
                interface,
                implementation,
                binding,
            } => JourneyAssumption {
                kind: AssumptionKind::HiltDispatch,
                step,
                screen: screen.to_owned(),
                symbol: implementation.clone(),
                interface: Some(interface.clone()),
                param: None,
                composable: None,
                registration: Some(binding.clone()),
                forwarded: Vec::new(),
                limitation: HILT_DISPATCH_LIMITATION.to_owned(),
            },
        }
    }
}

fn extract_method(edge: &Edge, endpoint: &Node) -> String {
    if let Some(m) = edge.props["method"].as_str().filter(|s| !s.is_empty()) {
        return m.to_ascii_uppercase();
    }
    if let Some(m) = endpoint.props["method"].as_str().filter(|s| !s.is_empty()) {
        return m.to_ascii_uppercase();
    }
    if let Some(name) = endpoint.props["name"].as_str()
        && let Some((m, _)) = name.split_once(' ')
    {
        return m.to_ascii_uppercase();
    }
    String::new()
}

fn is_assumed_write_method(method: &str) -> bool {
    matches!(method, "POST" | "PUT" | "PATCH" | "DELETE")
}

fn endpoint_path<'a>(endpoint: &'a Node, method: &str) -> &'a str {
    if let Some(p) = endpoint.props["path"].as_str().filter(|s| !s.is_empty()) {
        return p;
    }
    if let Some(name) = endpoint.props["name"].as_str().filter(|s| !s.is_empty()) {
        if let Some((prefix, rest)) = name.split_once(' ')
            && prefix.eq_ignore_ascii_case(method)
        {
            return rest.trim();
        }
        return name;
    }
    endpoint
        .id
        .split_once('@')
        .map_or(endpoint.id.as_str(), |(_, rest)| rest)
}

fn screen_display_label(screen: &Node, renders_by_screen: &BTreeMap<&str, Vec<&Node>>) -> String {
    // A server-rendered page is named by its route's endpoint name.
    if screen.props["server_rendered"].as_bool() == Some(true)
        && let Some(name) = screen.props["name"].as_str()
    {
        return name.to_owned();
    }
    if let Some(targets) = renders_by_screen.get(screen.id.as_str()) {
        for target in targets {
            let raw = target.props["path"]
                .as_str()
                .or_else(|| target.props["name"].as_str())
                .unwrap_or_else(|| {
                    target
                        .id
                        .split_once('@')
                        .map_or(target.id.as_str(), |(_, rest)| rest)
                });
            let after_hash = raw.rsplit_once('#').map_or(raw, |(_, sym)| sym);
            let segment = after_hash
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(after_hash);
            let stem = segment
                .strip_suffix(".vue")
                .or_else(|| segment.strip_suffix(".tsx"))
                .or_else(|| segment.strip_suffix(".ts"))
                .or_else(|| segment.strip_suffix(".jsx"))
                .or_else(|| segment.strip_suffix(".js"))
                .unwrap_or(segment);
            if !stem.is_empty() {
                return stem.to_owned();
            }
        }
    }
    screen.props["route"]
        .as_str()
        .or_else(|| screen.props["name"].as_str())
        .unwrap_or(screen.id.as_str())
        .to_owned()
}

#[derive(Clone)]
struct OutgoingNav<'a> {
    edge: &'a Edge,
    source_symbol: String,
    target_screen: String,
    sites: Vec<NavigationSite>,
    assumption: Option<NavAssumption>,
}

#[derive(Clone)]
struct ScreenClosure<'a> {
    calls: Vec<(StepCall, &'a Edge)>,
    unresolved: Vec<UnresolvedStepItem>,
    unread_callbacks: Vec<UnreadCallback>,
    outgoing_navs: Vec<OutgoingNav<'a>>,
    /// Every read registration whose callback reaches a Symbol that proven
    /// code does not, in record order.
    symbol_assumptions: BTreeMap<String, Vec<NavAssumption>>,
    calls_truncated: bool,
}

#[expect(clippy::too_many_arguments)]
fn try_visit_symbol<'a>(
    sym_id: &str,
    repo: &str,
    by_id: &BTreeMap<&'a str, &'a Node>,
    budget: JourneyBudget,
    visited: &mut Vec<(&'a str, Option<NavAssumption>)>,
    visited_set: &mut BTreeSet<&'a str>,
    queue: &mut VecDeque<&'a str>,
    calls_truncated: &mut bool,
    assumption: Option<NavAssumption>,
) {
    if visited_set.contains(sym_id) {
        return;
    }
    let Some((&sym_id, &sym_node)) = by_id.get_key_value(sym_id) else {
        return;
    };
    // A server-rendered page's templates are walked like its code (AC-0855).
    if !matches!(sym_node.label.as_str(), "Symbol" | "Template")
        || is_gap_node(sym_node)
        || !same_repo(repo, sym_node)
    {
        return;
    }
    if visited.len() >= budget.max_calls_per_screen {
        *calls_truncated = true;
        return;
    }
    visited_set.insert(sym_id);
    visited.push((sym_id, assumption));
    queue.push_back(sym_id);
}

#[allow(clippy::too_many_arguments)]
fn drain_symbol_queue<'a>(
    repo: &str,
    by_id: &BTreeMap<&'a str, &'a Node>,
    edges: &[&'a Edge],
    budget: JourneyBudget,
    visited: &mut Vec<(&'a str, Option<NavAssumption>)>,
    visited_set: &mut BTreeSet<&'a str>,
    queue: &mut VecDeque<&'a str>,
    calls_truncated: &mut bool,
) {
    while let Some(sym_id) = queue.pop_front() {
        let mut callees: Vec<&'a str> = edges
            .iter()
            .copied()
            .filter(|e| e.src == sym_id && is_proven_call(e, by_id))
            .map(|e| e.dst.as_str())
            .collect();
        callees.sort_unstable();
        callees.dedup();
        for callee_id in callees {
            try_visit_symbol(
                callee_id,
                repo,
                by_id,
                budget,
                visited,
                visited_set,
                queue,
                calls_truncated,
                None,
            );
        }
    }
}

fn build_screen_closure<'a>(
    screen: &'a Node,
    by_id: &BTreeMap<&'a str, &'a Node>,
    edges: &[&'a Edge],
    defined_in_file: &BTreeMap<&'a str, Vec<&'a str>>,
    budget: JourneyBudget,
) -> ScreenClosure<'a> {
    let repo = screen_repo(&screen.id);
    let mut visited: Vec<(&'a str, Option<NavAssumption>)> = Vec::new();
    let mut visited_set: BTreeSet<&'a str> = BTreeSet::new();
    let mut queue: VecDeque<&'a str> = VecDeque::new();
    let mut calls_truncated = false;

    let mut renders_targets: Vec<&'a str> = edges
        .iter()
        .copied()
        .filter(|e| e.src == screen.id && e.label == "RENDERS" && !is_gap_edge(e, by_id))
        .map(|e| e.dst.as_str())
        .collect();
    renders_targets.sort_unstable();
    renders_targets.dedup();

    for target_id in renders_targets {
        let Some(target) = by_id.get(target_id).copied() else {
            continue;
        };
        if is_gap_node(target) {
            continue;
        }
        match target.label.as_str() {
            "Symbol" => try_visit_symbol(
                target.id.as_str(),
                repo,
                by_id,
                budget,
                &mut visited,
                &mut visited_set,
                &mut queue,
                &mut calls_truncated,
                None,
            ),
            "File" => {
                if let Some(syms) = defined_in_file.get(target.id.as_str()) {
                    for &sym_id in syms {
                        try_visit_symbol(
                            sym_id,
                            repo,
                            by_id,
                            budget,
                            &mut visited,
                            &mut visited_set,
                            &mut queue,
                            &mut calls_truncated,
                            None,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    drain_symbol_queue(
        repo,
        by_id,
        edges,
        budget,
        &mut visited,
        &mut visited_set,
        &mut queue,
        &mut calls_truncated,
    );

    let screen_prov = provenance(&screen.props, &screen.id);
    let default_repo = screen_prov
        .evidence
        .first()
        .map(|e| e.repo.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(repo);
    let default_sha = screen_prov
        .evidence
        .first()
        .map(|e| e.commit_sha.as_str())
        .unwrap_or("");

    // Proven code dominates: a Symbol it reaches never carries an assumption.
    let proven: BTreeSet<&'a str> = visited_set.clone();
    let mut symbol_assumptions: BTreeMap<String, Vec<NavAssumption>> = BTreeMap::new();
    let mut unread_callbacks: Vec<UnreadCallback> = Vec::new();
    if let Some(records) = screen.props["entry_callbacks"].as_array() {
        for rec in records {
            let Some(param) = rec["param"].as_str() else {
                continue;
            };
            let Some(symbol) = rec["symbol"].as_str() else {
                continue;
            };
            let Some(composable) = rec["composable"].as_str() else {
                continue;
            };
            let Some(registration) =
                parse_callback_evidence(&rec["evidence"], default_repo, default_sha)
            else {
                continue;
            };
            let read = rec["read"].as_bool().unwrap_or(false);
            if !read {
                unread_callbacks.push(UnreadCallback {
                    param: param.to_owned(),
                    composable: composable.to_owned(),
                    symbol: symbol.to_owned(),
                    registration,
                });
                continue;
            }
            let forwarded: Vec<ForwardHop> = rec["forwarded"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|hop| {
                    Some(ForwardHop {
                        composable: hop["composable"].as_str()?.to_owned(),
                        param: hop["param"].as_str()?.to_owned(),
                    })
                })
                .collect();
            let assumption = NavAssumption::EntryCallback(EntryCallbackRecord {
                param: param.to_owned(),
                symbol: symbol.to_owned(),
                composable: composable.to_owned(),
                registration,
                forwarded,
            });
            // Each registration keeps its own reach, so two registrations of
            // one callback (or converging callees) both support what they reach.
            let mut reach: BTreeSet<&'a str> = BTreeSet::new();
            let mut pending: VecDeque<&str> = VecDeque::from([symbol]);
            while let Some(sym) = pending.pop_front() {
                if proven.contains(sym) || reach.contains(sym) {
                    continue;
                }
                try_visit_symbol(
                    sym,
                    repo,
                    by_id,
                    budget,
                    &mut visited,
                    &mut visited_set,
                    &mut queue,
                    &mut calls_truncated,
                    Some(assumption.clone()),
                );
                queue.clear();
                // Only Symbols admitted under the shared budget are followed.
                let Some((&sym, _)) = by_id.get_key_value(sym) else {
                    continue;
                };
                if !visited_set.contains(sym) {
                    continue;
                }
                reach.insert(sym);
                symbol_assumptions
                    .entry(sym.to_owned())
                    .or_default()
                    .push(assumption.clone());
                let mut callees: Vec<&'a str> = edges
                    .iter()
                    .copied()
                    .filter(|e| e.src == sym && is_proven_call(e, by_id))
                    .map(|e| e.dst.as_str())
                    .collect();
                callees.sort_unstable();
                callees.dedup();
                pending.extend(callees);
            }
        }
    }

    // Hilt dispatch (W11c-3): an interface member reaches its bound
    // implementation only as an assumption. What that reaches inherits it,
    // with any assumption already on the interface member, and a nested
    // dispatch adds its own. Proven code still dominates.
    let starts: Vec<&'a str> = visited.iter().map(|(sym, _)| *sym).collect();
    for start in starts {
        let inherited = symbol_assumptions.get(start).cloned().unwrap_or_default();
        let mut hops: Vec<&'a Edge> = edges
            .iter()
            .copied()
            .filter(|e| e.src == start && is_dispatch_edge(e) && !is_gap_edge(e, by_id))
            .collect();
        hops.sort_unstable_by(|a, b| a.dst.cmp(&b.dst));
        for hop in hops {
            let Some(assumption) = dispatch_assumption(hop) else {
                continue;
            };
            let mut carried = inherited.clone();
            carried.push(assumption);
            // Level by level: a dispatch's ordinary-call closure is complete
            // before any nested dispatch starts, so a symbol keeps the
            // fewest assumptions any route reaches it with.
            let mut reach: BTreeSet<&'a str> = BTreeSet::new();
            let mut levels: VecDeque<(&'a str, Vec<NavAssumption>)> =
                VecDeque::from([(hop.dst.as_str(), carried)]);
            while let Some((root, carried)) = levels.pop_front() {
                let mut pending: VecDeque<&'a str> = VecDeque::from([root]);
                while let Some(sym) = pending.pop_front() {
                    if proven.contains(sym) || reach.contains(sym) {
                        continue;
                    }
                    try_visit_symbol(
                        sym,
                        repo,
                        by_id,
                        budget,
                        &mut visited,
                        &mut visited_set,
                        &mut queue,
                        &mut calls_truncated,
                        carried.first().cloned(),
                    );
                    queue.clear();
                    if !visited_set.contains(sym) {
                        continue;
                    }
                    reach.insert(sym);
                    let recorded = symbol_assumptions.entry(sym.to_owned()).or_default();
                    for a in &carried {
                        if !recorded.contains(a) {
                            recorded.push(a.clone());
                        }
                    }
                    let mut next: Vec<&'a Edge> = edges
                        .iter()
                        .copied()
                        .filter(|e| e.src == sym && e.label == "CALLS" && !is_gap_edge(e, by_id))
                        .collect();
                    next.sort_unstable_by(|a, b| a.dst.cmp(&b.dst));
                    for edge in next {
                        if !is_dispatch_edge(edge) {
                            pending.push_back(edge.dst.as_str());
                            continue;
                        }
                        let Some(nested) = dispatch_assumption(edge) else {
                            continue;
                        };
                        let mut deeper = carried.clone();
                        deeper.push(nested);
                        levels.push_back((edge.dst.as_str(), deeper));
                    }
                }
            }
        }
    }

    let mut calls: Vec<(StepCall, &'a Edge)> = Vec::new();
    let mut unresolved: Vec<UnresolvedStepItem> = Vec::new();
    let mut outgoing_navs: Vec<OutgoingNav<'a>> = Vec::new();
    let mut seen_navs: BTreeSet<(String, String)> = BTreeSet::new();

    for (sym_id, _) in &visited {
        let sym_id = *sym_id;
        let sym_assumption = symbol_assumptions
            .get(sym_id)
            .and_then(|all| all.first())
            .cloned();
        for &edge in edges {
            if edge.src != sym_id {
                continue;
            }
            match edge.label.as_str() {
                // A server-rendered form `SUBMITS` like a client call (AC-0855).
                "FETCHES" | "SUBMITS" => {
                    if let Some(dst) = by_id.get(edge.dst.as_str()).copied() {
                        if is_gap_node(dst) || edge_confidence(edge) == ConfidenceTier::Gap {
                            unresolved.push(UnresolvedStepItem {
                                kind: UnresolvedKind::Call,
                                source: sym_id.to_owned(),
                                gap_id: dst.id.clone(),
                            });
                        } else if dst.label == "Endpoint" {
                            let method = extract_method(edge, dst);
                            let prov = provenance(
                                &edge.props,
                                &format!("{} {} {}", edge.src, edge.label, edge.dst),
                            );
                            let evidence = prov.evidence.into_iter().next();
                            calls.push((
                                StepCall {
                                    endpoint: dst.id.clone(),
                                    method,
                                    source: sym_id.to_owned(),
                                    evidence,
                                    straight_line: edge.props["straight_line"].as_bool(),
                                },
                                edge,
                            ));
                        }
                    } else if edge_confidence(edge) == ConfidenceTier::Gap {
                        unresolved.push(UnresolvedStepItem {
                            kind: UnresolvedKind::Call,
                            source: sym_id.to_owned(),
                            gap_id: edge.dst.clone(),
                        });
                    }
                }
                "NAVIGATES_TO" => {
                    if let Some(dst) = by_id.get(edge.dst.as_str()).copied() {
                        if is_gap_node(dst) || edge_confidence(edge) == ConfidenceTier::Gap {
                            unresolved.push(UnresolvedStepItem {
                                kind: UnresolvedKind::Navigation,
                                source: sym_id.to_owned(),
                                gap_id: dst.id.clone(),
                            });
                        } else if dst.label == "Screen"
                            && seen_navs.insert((sym_id.to_owned(), dst.id.clone()))
                        {
                            outgoing_navs.push(OutgoingNav {
                                edge,
                                source_symbol: sym_id.to_owned(),
                                target_screen: dst.id.clone(),
                                sites: parse_navigation_sites(edge),
                                assumption: sym_assumption.clone(),
                            });
                        }
                    } else if edge_confidence(edge) == ConfidenceTier::Gap {
                        unresolved.push(UnresolvedStepItem {
                            kind: UnresolvedKind::Navigation,
                            source: sym_id.to_owned(),
                            gap_id: edge.dst.clone(),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    if screen.props["initial"].as_bool() == Some(true) {
        let owners: Vec<&str> = screen.props["initial_owners"]
            .as_array()
            .filter(|arr| !arr.is_empty())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .or_else(|| {
                screen.props["back_stack_owner"]
                    .as_str()
                    .map(|owner| vec![owner])
            })
            .unwrap_or_default();
        for owner in owners {
            if by_id
                .get(owner)
                .is_some_and(|n| n.label != "Symbol" || is_gap_node(n) || !same_repo(repo, n))
            {
                continue;
            }
            for &edge in edges {
                if edge.src != owner
                    || edge.label != "NAVIGATES_TO"
                    || edge_confidence(edge) == ConfidenceTier::Gap
                {
                    continue;
                }
                let Some(dst) = by_id.get(edge.dst.as_str()).copied() else {
                    continue;
                };
                if dst.label != "Screen" || is_gap_node(dst) {
                    continue;
                }
                let init_sites = parse_init_navigation_sites(edge);
                if init_sites.is_empty() {
                    continue;
                }
                if seen_navs.insert((owner.to_owned(), dst.id.clone())) {
                    outgoing_navs.push(OutgoingNav {
                        edge,
                        source_symbol: owner.to_owned(),
                        target_screen: dst.id.clone(),
                        sites: init_sites,
                        assumption: Some(NavAssumption::Startup {
                            owner: owner.to_owned(),
                        }),
                    });
                }
            }
        }
    }

    calls.sort_by(|(a, _), (b, _)| {
        let a_span = a
            .evidence
            .as_ref()
            .map_or((u64::MAX, u64::MAX), |ev| (ev.byte_start, ev.byte_end));
        let b_span = b
            .evidence
            .as_ref()
            .map_or((u64::MAX, u64::MAX), |ev| (ev.byte_start, ev.byte_end));
        (a_span, &a.endpoint, &a.source, &a.method).cmp(&(
            b_span,
            &b.endpoint,
            &b.source,
            &b.method,
        ))
    });
    unresolved.sort();
    unresolved.dedup();
    outgoing_navs.sort_by(|a, b| {
        (&a.target_screen, &a.source_symbol).cmp(&(&b.target_screen, &b.source_symbol))
    });

    ScreenClosure {
        calls,
        unresolved,
        unread_callbacks,
        outgoing_navs,
        symbol_assumptions,
        calls_truncated,
    }
}

#[derive(Clone)]
struct PathNode<'a> {
    screen: String,
    nav: Option<StepNavigation>,
    nav_edge: Option<&'a Edge>,
    nav_assumption: Option<NavAssumption>,
}

fn rank_confidence(c: ConfidenceTier) -> u8 {
    match c {
        ConfidenceTier::Gap => 0,
        ConfidenceTier::InferredWeak => 1,
        ConfidenceTier::InferredStrong => 2,
        ConfidenceTier::Confirmed => 3,
    }
}

fn weakest_confidence(tiers: impl IntoIterator<Item = ConfidenceTier>) -> ConfidenceTier {
    let mut min_rank = rank_confidence(ConfidenceTier::InferredWeak);
    for t in tiers {
        min_rank = min_rank.min(rank_confidence(t));
    }
    match min_rank {
        0 => ConfidenceTier::Gap,
        _ => ConfidenceTier::InferredWeak,
    }
}

/// Observed writes of `participants`, recorded on `endpoint`.
fn record_writes<'a>(
    writes_by_endpoint: &mut BTreeMap<&'a str, Vec<(WriteFact, ConfidenceTier)>>,
    writes_by_symbol: &BTreeMap<&'a str, Vec<(&'a str, &'a Edge, &'a Node)>>,
    endpoint: &'a str,
    participants: &BTreeMap<&'a str, ConfidenceTier>,
) {
    let entry = writes_by_endpoint.entry(endpoint).or_default();
    for (&sym_id, &reach) in participants {
        if let Some(writes) = writes_by_symbol.get(sym_id) {
            for &(data_id, write_edge, data_node) in writes {
                let tier = weakest_confidence([
                    reach,
                    edge_confidence(write_edge),
                    provenance(&data_node.props, &data_node.id).confidence_tier,
                ]);
                entry.push((
                    WriteFact {
                        symbol: sym_id.to_owned(),
                        data_entity: data_id.to_owned(),
                    },
                    tier,
                ));
            }
        }
    }
    entry.sort_by(|a, b| a.0.cmp(&b.0));
    entry.dedup_by(|a, b| a.0 == b.0);
}

/// Recover bounded T2 journey candidates (`journey-candidate@2`) from entry
/// screens (`Screen` nodes with `props.route == "/"` or `props.initial == true`).
///
/// Output order is deterministic: sorted by entry screen repo and id, then by
/// candidate `id` and `goal.endpoint`.
pub fn candidates(
    nodes: &[&Node],
    edges: &[&Edge],
    flows: &[Flow],
    budget: JourneyBudget,
) -> Vec<JourneyCandidate> {
    search(nodes, edges, flows, budget).candidates
}

/// Run the bounded `journey-candidate@2` search across all entry screens (`Screen`
/// nodes with `props.route == "/"` or `props.initial == true`), returning both the
/// recovered [`JourneyCandidate`] values and per-entry [`EntrySearchSummary`] rows.
pub fn search(
    nodes: &[&Node],
    edges: &[&Edge],
    flows: &[Flow],
    budget: JourneyBudget,
) -> JourneySearch {
    let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), *n)).collect();

    // Map File id -> sorted Symbol ids with DEFINED_IN edges to that File.
    let mut defined_in_file: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    // Map Screen id -> RENDERS target nodes for display labels.
    let mut renders_by_screen: BTreeMap<&str, Vec<&Node>> = BTreeMap::new();
    // Map Symbol id -> sorted DataEntity ids with WRITES edges from that Symbol.
    let mut writes_by_symbol: BTreeMap<&str, Vec<(&str, &Edge, &Node)>> = BTreeMap::new();

    for &edge in edges {
        if is_gap_edge(edge, &by_id) {
            continue;
        }
        match edge.label.as_str() {
            "DEFINED_IN" => {
                if let (Some(src), Some(dst)) =
                    (by_id.get(edge.src.as_str()), by_id.get(edge.dst.as_str()))
                    && src.label == "Symbol"
                    && dst.label == "File"
                    && !is_gap_node(src)
                    && !is_gap_node(dst)
                {
                    defined_in_file
                        .entry(dst.id.as_str())
                        .or_default()
                        .push(src.id.as_str());
                }
            }
            "RENDERS" => {
                if let (Some(src), Some(dst)) =
                    (by_id.get(edge.src.as_str()), by_id.get(edge.dst.as_str()))
                    && src.label == "Screen"
                    && !is_gap_node(src)
                    && !is_gap_node(dst)
                {
                    renders_by_screen
                        .entry(src.id.as_str())
                        .or_default()
                        .push(*dst);
                }
            }
            "WRITES" => {
                if let (Some(src), Some(dst)) =
                    (by_id.get(edge.src.as_str()), by_id.get(edge.dst.as_str()))
                    && src.label == "Symbol"
                    && dst.label == "DataEntity"
                    && !is_gap_node(src)
                    && !is_gap_node(dst)
                {
                    writes_by_symbol.entry(src.id.as_str()).or_default().push((
                        dst.id.as_str(),
                        edge,
                        *dst,
                    ));
                }
            }
            _ => {}
        }
    }
    for syms in defined_in_file.values_mut() {
        syms.sort_unstable();
        syms.dedup();
    }
    for targets in renders_by_screen.values_mut() {
        targets.sort_by(|a, b| a.id.cmp(&b.id));
        targets.dedup_by(|a, b| a.id == b.id);
    }
    for writes in writes_by_symbol.values_mut() {
        writes.sort_by(|a, b| a.0.cmp(b.0));
        writes.dedup_by(|a, b| a.0 == b.0);
    }

    // Map Endpoint id -> observed WriteFacts from flows triggered by that Endpoint.
    let mut writes_by_endpoint: BTreeMap<&str, Vec<(WriteFact, ConfidenceTier)>> = BTreeMap::new();
    let mut flows_by_endpoint: BTreeMap<&str, Vec<&Flow>> = BTreeMap::new();
    for flow in flows {
        flows_by_endpoint
            .entry(flow.trigger.as_str())
            .or_default()
            .push(flow);
        // The trigger's own flow: its hop tiers are the flow's to report.
        let mut participants: BTreeMap<&str, ConfidenceTier> = BTreeMap::new();
        for hop in &flow.hops {
            if hop.provenance.confidence_tier == ConfidenceTier::Gap {
                continue;
            }
            participants.insert(hop.src.as_str(), ConfidenceTier::Confirmed);
            participants.insert(hop.dst.as_str(), ConfidenceTier::Confirmed);
        }
        record_writes(
            &mut writes_by_endpoint,
            &writes_by_symbol,
            flow.trigger.as_str(),
            &participants,
        );
        // An endpoint an anchor's flow fetches gets no flow of its own
        // (#1066): its writes are what the flow reaches downstream of it.
        let mut next: BTreeMap<&str, Vec<(&str, ConfidenceTier)>> = BTreeMap::new();
        for hop in &flow.hops {
            if hop.provenance.confidence_tier != ConfidenceTier::Gap {
                next.entry(hop.src.as_str())
                    .or_default()
                    .push((hop.dst.as_str(), hop.provenance.confidence_tier));
            }
        }
        let fetched: BTreeSet<&str> = flow
            .hops
            .iter()
            .filter(|hop| {
                hop.label == "FETCHES"
                    && hop.provenance.confidence_tier != ConfidenceTier::Gap
                    && hop.dst != flow.trigger
                    && by_id
                        .get(hop.dst.as_str())
                        .is_some_and(|n| n.label == "Endpoint")
            })
            .map(|hop| hop.dst.as_str())
            .collect();
        for endpoint in fetched {
            // Each downstream node keeps its strongest path's weakest hop,
            // so an attributed write carries the reach it rests on.
            let mut downstream: BTreeMap<&str, ConfidenceTier> = BTreeMap::new();
            let mut pending = vec![(endpoint, ConfidenceTier::Confirmed)];
            while let Some((id, reach)) = pending.pop() {
                if downstream
                    .get(id)
                    .is_some_and(|&best| rank_confidence(best) >= rank_confidence(reach))
                {
                    continue;
                }
                downstream.insert(id, reach);
                for &(dst, tier) in next.get(id).into_iter().flatten() {
                    let hop_reach = if rank_confidence(tier) < rank_confidence(reach) {
                        tier
                    } else {
                        reach
                    };
                    pending.push((dst, hop_reach));
                }
            }
            record_writes(
                &mut writes_by_endpoint,
                &writes_by_symbol,
                endpoint,
                &downstream,
            );
        }
    }

    let test_refs = TestReferences::new(nodes, edges, |e| !is_gap_edge(e, &by_id));

    // Precompute closures for all non-Gap Screen nodes.
    let mut closures: BTreeMap<&str, ScreenClosure<'_>> = BTreeMap::new();
    for &node in nodes {
        if node.label == "Screen" && !is_gap_node(node) {
            closures.insert(
                node.id.as_str(),
                build_screen_closure(node, &by_id, edges, &defined_in_file, budget),
            );
        }
    }

    // Entry screens: non-Gap Screen nodes with props.route == "/" or props.initial == true, sorted by (repo, id).
    let mut entries: Vec<&Node> = nodes
        .iter()
        .copied()
        .filter(|n| {
            n.label == "Screen"
                && !is_gap_node(n)
                && (n.props["route"].as_str() == Some("/")
                    || n.props["initial"].as_bool() == Some(true))
        })
        .collect();
    entries.sort_by(|a, b| (screen_repo(&a.id), &a.id).cmp(&(screen_repo(&b.id), &b.id)));

    let mut out_candidates = Vec::new();
    let mut out_entries = Vec::new();
    for entry in entries {
        let (entry_candidates, summary) = search_entry(
            entry,
            &by_id,
            &closures,
            &renders_by_screen,
            &writes_by_endpoint,
            &flows_by_endpoint,
            test_refs.as_ref(),
            budget,
        );
        out_candidates.extend(entry_candidates);
        out_entries.push(summary);
    }
    JourneySearch {
        candidates: out_candidates,
        entries: out_entries,
    }
}

#[derive(Clone)]
struct RawCandidate<'a> {
    path: Vec<PathNode<'a>>,
    goal_call: StepCall,
    goal_edge: &'a Edge,
    goal_basis: GoalBasis,
    writes: Vec<(WriteFact, ConfidenceTier)>,
    earlier_assumed_goals: Vec<AssumedGoal>,
    calls_truncated: bool,
    /// Whether the path crosses a screen with an observed-write goal before
    /// its goal screen (ADR-0084).
    crossed_write: bool,
}

type ObservedGoal<'a> = (StepCall, &'a Edge, Vec<(WriteFact, ConfidenceTier)>);

/// A step's goals in fetch order: every observed-write call, each with the
/// method-assumed calls before it; all method-assumed calls; and the first.
#[expect(clippy::type_complexity)]
fn step_goal_classification<'a>(
    screen_id: &str,
    closure: &ScreenClosure<'a>,
    writes_by_endpoint: &BTreeMap<&str, Vec<(WriteFact, ConfidenceTier)>>,
) -> (
    Vec<(ObservedGoal<'a>, Vec<AssumedGoal>)>,
    Vec<AssumedGoal>,
    Option<(StepCall, &'a Edge)>,
) {
    let mut observed: Vec<(ObservedGoal<'a>, Vec<AssumedGoal>)> = Vec::new();
    let mut assumed: Vec<AssumedGoal> = Vec::new();
    let mut first_assumed: Option<(StepCall, &'a Edge)> = None;

    for (call, edge) in &closure.calls {
        let writes = writes_by_endpoint
            .get(call.endpoint.as_str())
            .filter(|w| !w.is_empty());
        if let Some(writes) = writes {
            observed.push(((call.clone(), *edge, writes.clone()), assumed.clone()));
        } else if is_assumed_write_method(&call.method) {
            if first_assumed.is_none() {
                first_assumed = Some((call.clone(), *edge));
            }
            assumed.push(AssumedGoal {
                screen: screen_id.to_owned(),
                endpoint: call.endpoint.clone(),
                method: call.method.clone(),
                source: call.source.clone(),
                write_coverage: "unknown".into(),
            });
        }
    }
    (observed, assumed, first_assumed)
}

#[expect(clippy::too_many_arguments)]
fn search_entry<'a>(
    entry: &'a Node,
    by_id: &BTreeMap<&'a str, &'a Node>,
    closures: &BTreeMap<&'a str, ScreenClosure<'a>>,
    renders_by_screen: &BTreeMap<&'a str, Vec<&'a Node>>,
    writes_by_endpoint: &BTreeMap<&'a str, Vec<(WriteFact, ConfidenceTier)>>,
    flows_by_endpoint: &BTreeMap<&'a str, Vec<&'a Flow>>,
    test_refs: Option<&TestReferences<'a>>,
    budget: JourneyBudget,
) -> (Vec<JourneyCandidate>, EntrySearchSummary) {
    if budget.max_screens == 0 || budget.max_candidates == 0 || budget.max_expanded_states == 0 {
        let truncated = if budget.max_screens == 0 {
            Some(TruncationReason::MaxScreens)
        } else if budget.max_expanded_states == 0 {
            Some(TruncationReason::ExpandedStates)
        } else {
            Some(TruncationReason::Candidates)
        };
        return (
            Vec::new(),
            EntrySearchSummary {
                entry: entry.id.clone(),
                expanded_states: 0,
                candidates: 0,
                truncated,
                assumed_navigations: 0,
                unread_callbacks: 0,
            },
        );
    }

    #[derive(Clone)]
    struct QueueState<'b> {
        path: Vec<PathNode<'b>>,
        earlier_assumed: Vec<AssumedGoal>,
        first_assumed_prefix: Option<(Vec<PathNode<'b>>, StepCall, &'b Edge, bool)>,
        calls_truncated: bool,
        crossed_write: bool,
        /// A write step whose goals were already scored, queued to expand.
        scored: bool,
    }

    // Two queues (ADR-0084): every path that crosses no observed write is
    // searched first, in the same order and with the same budget use as
    // `journey-candidate@1`; write steps and the paths past them wait in
    // `deferred` and spend only the budget left after that.
    let mut queue: VecDeque<QueueState<'a>> = VecDeque::new();
    let mut deferred: VecDeque<QueueState<'a>> = VecDeque::new();
    queue.push_back(QueueState {
        path: vec![PathNode {
            screen: entry.id.clone(),
            nav: None,
            nav_edge: None,
            nav_assumption: None,
        }],
        earlier_assumed: Vec::new(),
        first_assumed_prefix: None,
        calls_truncated: false,
        crossed_write: false,
        scored: false,
    });

    let mut expanded_states = 0usize;
    let mut expanded_screens: BTreeSet<&'a str> = BTreeSet::new();
    let mut state_budget_hit = false;
    let mut screens_cut = false;
    let mut first_truncated: Option<TruncationReason> = None;

    // Observed-write paths found, and assumed-only prefixes tracked by their prefix screen/nav key.
    let mut observed_candidates: Vec<RawCandidate<'a>> = Vec::new();
    #[expect(clippy::type_complexity)]
    let mut assumed_prefixes: BTreeMap<
        (Vec<String>, Vec<(String, String)>, String),
        (RawCandidate<'a>, bool),
    > = BTreeMap::new();

    while let Some(state) = queue.pop_front().or_else(|| deferred.pop_front()) {
        let current_screen = state
            .path
            .last()
            .expect("path has at least entry screen")
            .screen
            .as_str();
        let Some((&screen_key, closure)) = closures.get_key_value(current_screen) else {
            continue;
        };
        if closure.calls_truncated && first_truncated.is_none() {
            first_truncated = Some(TruncationReason::CallsPerScreen);
        }
        let calls_truncated = state.calls_truncated || closure.calls_truncated;

        let (observed, step_assumed, first_step_assumed) =
            step_goal_classification(current_screen, closure, writes_by_endpoint);

        // A step with an observed write yields one candidate per observed
        // goal it fetches. Under `journey-candidate@2` the path goes on to
        // other goals past it (ADR-0084); the step's own goals end there.
        let write_step = !observed.is_empty();
        if write_step && !state.scored {
            if let Some((prefix_path, assumed_call, _, _)) = &state.first_assumed_prefix {
                let key = prefix_key(prefix_path, &assumed_call.endpoint);
                if let Some(entry) = assumed_prefixes.get_mut(&key) {
                    entry.1 = true; // A descendant reached an observed write.
                }
            }
            for ((goal_call, goal_edge, writes), before) in observed {
                let mut earlier = state.earlier_assumed.clone();
                earlier.extend(before);
                observed_candidates.push(RawCandidate {
                    path: state.path.clone(),
                    goal_call,
                    goal_edge,
                    goal_basis: GoalBasis::ObservedWrite,
                    writes,
                    earlier_assumed_goals: earlier,
                    calls_truncated,
                    crossed_write: state.crossed_write,
                });
            }
            deferred.push_back(QueueState {
                scored: true,
                ..state
            });
            continue;
        }

        let mut next_earlier = state.earlier_assumed.clone();
        next_earlier.extend(step_assumed);

        // A write step's method-assumed calls stay earlier goals of its
        // observed candidates, never assumed candidates of their own.
        let first_step_assumed = first_step_assumed.filter(|_| !write_step);
        let next_first_assumed = match state.first_assumed_prefix {
            Some(existing) => Some(existing),
            None => first_step_assumed.map(|(call, edge)| {
                let key = prefix_key(&state.path, &call.endpoint);
                assumed_prefixes.entry(key).or_insert_with(|| {
                    (
                        RawCandidate {
                            path: state.path.clone(),
                            goal_call: call.clone(),
                            goal_edge: edge,
                            goal_basis: GoalBasis::MethodAssumed,
                            writes: Vec::new(),
                            earlier_assumed_goals: Vec::new(),
                            calls_truncated,
                            crossed_write: state.crossed_write,
                        },
                        false,
                    )
                });
                (state.path.clone(), call, edge, calls_truncated)
            }),
        };

        let unvisited = || {
            closure
                .outgoing_navs
                .iter()
                .any(|nav| !state.path.iter().any(|p| p.screen == nav.target_screen))
        };
        if state.path.len() >= budget.max_screens {
            if unvisited() {
                screens_cut = true;
                if first_truncated.is_none() {
                    first_truncated = Some(TruncationReason::MaxScreens);
                }
            }
            continue;
        }

        // Queued states are still scored for goals once the budget is spent; only
        // expansion stops, and only a state with an unvisited target marks the cut.
        if expanded_states >= budget.max_expanded_states {
            if unvisited() {
                state_budget_hit = true;
                if first_truncated.is_none() {
                    first_truncated = Some(TruncationReason::ExpandedStates);
                }
            }
            continue;
        }
        expanded_states += 1;
        expanded_screens.insert(screen_key);

        for nav in &closure.outgoing_navs {
            if state.path.iter().any(|p| p.screen == nav.target_screen) {
                continue;
            }
            let mut next_path = state.path.clone();
            next_path.push(PathNode {
                screen: nav.target_screen.clone(),
                nav: Some(StepNavigation {
                    source: nav.source_symbol.clone(),
                    target: nav.target_screen.clone(),
                    sites_taken: nav.sites.clone(),
                }),
                nav_edge: Some(nav.edge),
                nav_assumption: nav.assumption.clone(),
            });
            let crossed_write = state.crossed_write || write_step;
            let next = QueueState {
                path: next_path,
                earlier_assumed: next_earlier.clone(),
                first_assumed_prefix: next_first_assumed.clone(),
                calls_truncated,
                crossed_write,
                scored: false,
            };
            if crossed_write {
                deferred.push_back(next);
            } else {
                queue.push_back(next);
            }
        }
    }

    let mut raw_all: Vec<RawCandidate<'a>> = observed_candidates;
    for (_, (raw, reached_observed_descendant)) in assumed_prefixes {
        if !reached_observed_descendant {
            raw_all.push(raw);
        }
    }

    // Group by goal endpoint (entry is fixed). The shortest path wins, ties
    // broken by step screen ids then navs; a path that crosses an earlier
    // observed-write screen ranks after every path that does not, so the
    // goal's `journey-candidate@1` path still wins where it exists.
    let mut by_goal: BTreeMap<String, Vec<RawCandidate<'a>>> = BTreeMap::new();
    for raw in raw_all {
        by_goal
            .entry(raw.goal_call.endpoint.clone())
            .or_default()
            .push(raw);
    }
    // Each goal: its shortest path, the other paths' count, and (ADR-0084)
    // its alternatives: in rank order, every path through a screen that no
    // better-ranked kept path of the goal passes through. That is the best
    // path through each screen the shortest one skips, each kept once.
    let mut ordered_goals: Vec<(RawCandidate<'a>, usize, Vec<RawCandidate<'a>>)> = by_goal
        .into_values()
        .map(|mut raws| {
            raws.sort_by(|a, b| rank(a).cmp(&rank(b)));
            let count = raws.len() - 1;
            let mut raws = raws.into_iter();
            let best = raws.next().expect("a goal has at least one path");
            let mut covered: BTreeSet<String> =
                best.path.iter().map(|p| p.screen.clone()).collect();
            let mut alternatives = Vec::new();
            for raw in raws {
                if raw.path.iter().any(|p| !covered.contains(&p.screen)) {
                    covered.extend(raw.path.iter().map(|p| p.screen.clone()));
                    alternatives.push(raw);
                }
            }
            (best, count, alternatives)
        })
        .collect();
    ordered_goals.sort_by(|(a, _, _), (b, _, _)| {
        (rank(a), &a.goal_call.endpoint).cmp(&(rank(b), &b.goal_call.endpoint))
    });

    let candidates_truncated = ordered_goals.len() > budget.max_candidates;
    if candidates_truncated {
        ordered_goals.truncate(budget.max_candidates);
        if first_truncated.is_none() {
            first_truncated = Some(TruncationReason::Candidates);
        }
    }

    // Either cut leaves paths unexplored; expanded states is the wider one.
    let search_cut = if state_budget_hit {
        Some(TruncationReason::ExpandedStates)
    } else if screens_cut {
        Some(TruncationReason::MaxScreens)
    } else {
        None
    };
    let finalize = |raw: RawCandidate<'a>, alternatives: usize, retained_cut| {
        finalize_candidate(
            entry,
            raw,
            alternatives,
            search_cut,
            retained_cut,
            by_id,
            closures,
            renders_by_screen,
            flows_by_endpoint,
            test_refs,
            budget,
        )
    };
    // Either retention cut applies to every candidate the entry keeps, so
    // both are known before any candidate is finalized.
    let alternatives_truncated = ordered_goals
        .iter()
        .map(|(_, _, alts)| alts.len())
        .sum::<usize>()
        > budget.max_alternatives;
    if alternatives_truncated && first_truncated.is_none() {
        first_truncated = Some(TruncationReason::Alternatives);
    }
    let retained_cut = if candidates_truncated {
        Some(TruncationReason::Candidates)
    } else {
        alternatives_truncated.then_some(TruncationReason::Alternatives)
    };
    let mut finalized: Vec<JourneyCandidate> = Vec::new();
    // Alternatives of the retained goals, best-ranked first across goals.
    let mut alternatives: Vec<(String, usize, RawCandidate<'a>)> = Vec::new();
    for (raw, count, alts) in ordered_goals {
        let primary = finalize(raw, count, retained_cut);
        alternatives.extend(alts.into_iter().map(|alt| (primary.id.clone(), count, alt)));
        finalized.push(primary);
    }
    alternatives.sort_by(|(_, _, a), (_, _, b)| {
        (rank(a), &a.goal_call.endpoint).cmp(&(rank(b), &b.goal_call.endpoint))
    });
    alternatives.truncate(budget.max_alternatives);
    for (primary, count, raw) in alternatives {
        let mut candidate = finalize(raw, count, retained_cut);
        candidate.alternative_of = Some(primary);
        finalized.push(candidate);
    }

    finalized.sort_by(|a, b| (&a.id, &a.goal.endpoint).cmp(&(&b.id, &b.goal.endpoint)));
    if first_truncated.is_none() {
        first_truncated = finalized.iter().find_map(|c| c.truncated);
    }

    let mut assumed_navigations = 0usize;
    let mut unread_callbacks = 0usize;
    for screen_id in expanded_screens {
        if let Some(closure) = closures.get(screen_id) {
            assumed_navigations += closure
                .outgoing_navs
                .iter()
                .filter(|nav| nav.assumption.is_some())
                .count();
            unread_callbacks += closure.unread_callbacks.len();
        }
    }

    let summary = EntrySearchSummary {
        entry: entry.id.clone(),
        expanded_states,
        candidates: finalized.len(),
        truncated: first_truncated,
        assumed_navigations,
        unread_callbacks,
    };
    (finalized, summary)
}

/// A goal's paths in `journey-candidate@2` order (ADR-0084): one that
/// crosses no earlier observed-write screen first, then [`path_order_key`].
#[expect(clippy::type_complexity)]
fn rank<'r>(
    raw: &'r RawCandidate<'_>,
) -> (bool, (usize, usize, Vec<&'r str>, Vec<(&'r str, &'r str)>)) {
    (raw.crossed_write, path_order_key(&raw.path))
}

fn prefix_key(
    path: &[PathNode<'_>],
    endpoint: &str,
) -> (Vec<String>, Vec<(String, String)>, String) {
    let screens = path.iter().map(|p| p.screen.clone()).collect();
    let navs = path
        .iter()
        .filter_map(|p| p.nav.as_ref().map(|n| (n.source.clone(), n.target.clone())))
        .collect();
    (screens, navs, endpoint.to_owned())
}

/// Shortest first; among equal lengths a path resting on fewer assumed
/// navigations wins (always zero for path routers), then step ids.
fn path_order_key<'a>(
    path: &'a [PathNode<'_>],
) -> (usize, usize, Vec<&'a str>, Vec<(&'a str, &'a str)>) {
    let assumed = path.iter().filter(|p| p.nav_assumption.is_some()).count();
    let screens: Vec<&'a str> = path.iter().map(|p| p.screen.as_str()).collect();
    let navs: Vec<(&'a str, &'a str)> = path
        .iter()
        .filter_map(|p| {
            p.nav
                .as_ref()
                .map(|n| (n.source.as_str(), n.target.as_str()))
        })
        .collect();
    (path.len(), assumed, screens, navs)
}

#[expect(clippy::too_many_arguments)]
fn finalize_candidate<'a>(
    entry: &'a Node,
    raw: RawCandidate<'a>,
    alternatives: usize,
    search_cut: Option<TruncationReason>,
    // `Candidates` or `Alternatives` when that retention bound cut the entry.
    retained_cut: Option<TruncationReason>,
    by_id: &BTreeMap<&'a str, &'a Node>,
    closures: &BTreeMap<&'a str, ScreenClosure<'a>>,
    renders_by_screen: &BTreeMap<&'a str, Vec<&'a Node>>,
    flows_by_endpoint: &BTreeMap<&'a str, Vec<&'a Flow>>,
    test_refs: Option<&TestReferences<'a>>,
    budget: JourneyBudget,
) -> JourneyCandidate {
    let repo = screen_repo(&entry.id);
    let mut steps: Vec<JourneyStep> = Vec::new();
    let mut nav_edge_props: Vec<Option<&serde_json::Value>> = Vec::new();
    let mut supporting_confidences: Vec<ConfidenceTier> = Vec::new();
    let mut citing_evidence: Vec<EvidenceRef> = Vec::new();
    let mut any_calls_truncated = raw.calls_truncated;
    let mut assumptions: Vec<JourneyAssumption> = Vec::new();

    for (idx, node) in raw.path.iter().enumerate() {
        if let Some(screen_node) = by_id.get(node.screen.as_str()) {
            supporting_confidences
                .push(provenance(&screen_node.props, &screen_node.id).confidence_tier);
        }
        if let (Some(nav), Some(edge)) = (&node.nav, node.nav_edge) {
            supporting_confidences.push(edge_confidence(edge));
            nav_edge_props.push(Some(&edge.props));
            citing_evidence.extend(navigation_evidence(edge, &nav.sites_taken));
        } else {
            nav_edge_props.push(None);
        }
        if idx > 0
            && let Some(assumption) = &node.nav_assumption
        {
            let prev = &raw.path[idx - 1].screen;
            let all = match (assumption, &node.nav) {
                (
                    NavAssumption::EntryCallback(_) | NavAssumption::HiltDispatch { .. },
                    Some(nav),
                ) => closures
                    .get(prev.as_str())
                    .and_then(|c| c.symbol_assumptions.get(&nav.source))
                    .cloned()
                    .unwrap_or_else(|| vec![assumption.clone()]),
                _ => vec![assumption.clone()],
            };
            for a in &all {
                assumptions.push(a.to_journey_assumption(idx, prev));
            }
        }
        let closure = closures.get(node.screen.as_str());
        if let Some(c) = closure {
            if c.calls_truncated {
                any_calls_truncated = true;
            }
            for (call, _) in &c.calls {
                for a in c.symbol_assumptions.get(&call.source).into_iter().flatten() {
                    assumptions.push(a.to_journey_assumption(idx, &node.screen));
                }
            }
            for item in &c.unresolved {
                for a in c.symbol_assumptions.get(&item.source).into_iter().flatten() {
                    assumptions.push(a.to_journey_assumption(idx, &node.screen));
                }
            }
        }
        steps.push(JourneyStep {
            screen: node.screen.clone(),
            navigation: node.nav.clone(),
            calls: closure
                .map(|c| c.calls.iter().map(|(call, _)| call.clone()).collect())
                .unwrap_or_default(),
            unresolved: closure.map(|c| c.unresolved.clone()).unwrap_or_default(),
            unread_callbacks: closure
                .map(|c| c.unread_callbacks.clone())
                .unwrap_or_default(),
        });
    }

    let goal_screen_id = raw
        .path
        .last()
        .expect("path has goal screen")
        .screen
        .clone();

    // Check for an ordered result step from the goal screen's closure.
    let (result, result_step, result_nav_assumption) = find_ordered_result_step(
        &goal_screen_id,
        &raw.goal_call,
        &raw.path,
        closures,
        by_id,
        &mut nav_edge_props,
        &mut supporting_confidences,
        &mut citing_evidence,
        &mut any_calls_truncated,
    );
    if let Some(res_step) = &result_step {
        let res_idx = steps.len();
        if let Some(assumption) = &result_nav_assumption {
            assumptions.push(assumption.to_journey_assumption(res_idx, &goal_screen_id));
        }
        if let Some(c) = closures.get(res_step.screen.as_str()) {
            for (call, _) in &c.calls {
                for a in c.symbol_assumptions.get(&call.source).into_iter().flatten() {
                    assumptions.push(a.to_journey_assumption(res_idx, &res_step.screen));
                }
            }
            for item in &c.unresolved {
                for a in c.symbol_assumptions.get(&item.source).into_iter().flatten() {
                    assumptions.push(a.to_journey_assumption(res_idx, &res_step.screen));
                }
            }
        }
        steps.push(res_step.clone());
    }

    let registration_key = |a: &JourneyAssumption| {
        a.registration
            .as_ref()
            .map(|r| (r.path.clone(), r.byte_start, r.byte_end))
    };
    assumptions.sort_by(|a, b| {
        (
            a.step,
            a.kind,
            &a.symbol,
            &a.param,
            &a.screen,
            &a.composable,
        )
            .cmp(&(
                b.step,
                b.kind,
                &b.symbol,
                &b.param,
                &b.screen,
                &b.composable,
            ))
            .then_with(|| registration_key(a).cmp(&registration_key(b)))
    });
    assumptions.dedup();
    // A registration is the only evidence tying a screen to its callback.
    for registration in assumptions.iter().filter_map(|a| a.registration.as_ref()) {
        if !citing_evidence.contains(registration) {
            citing_evidence.push(registration.clone());
        }
    }

    supporting_confidences.push(edge_confidence(raw.goal_edge));
    if let Some(ep_node) = by_id.get(raw.goal_call.endpoint.as_str()) {
        supporting_confidences.push(provenance(&ep_node.props, &ep_node.id).confidence_tier);
    }
    for (_, tier) in &raw.writes {
        supporting_confidences.push(*tier);
    }
    if let Some(ev) = &raw.goal_call.evidence {
        citing_evidence.push(ev.clone());
    }

    let conditional_steps: Vec<usize> = steps
        .iter()
        .enumerate()
        .filter_map(|(idx, step)| {
            let nav = step.navigation.as_ref()?;
            (!nav.sites_taken.is_empty() && nav.sites_taken.iter().all(|s| s.conditional))
                .then_some(idx)
        })
        .collect();

    let truncated = if any_calls_truncated {
        Some(TruncationReason::CallsPerScreen)
    } else {
        retained_cut.or(search_cut)
    };
    let alternatives_complete =
        search_cut.is_none() && !any_calls_truncated && retained_cut.is_none();

    let endpoint_flows = flows_by_endpoint
        .get(raw.goal_call.endpoint.as_str())
        .cloned()
        .unwrap_or_default();
    let mut flow_triggers: Vec<String> = endpoint_flows.iter().map(|f| f.trigger.clone()).collect();
    flow_triggers.sort();
    flow_triggers.dedup();

    let mut tests: BTreeSet<String> = BTreeSet::new();
    if let Some(refs) = test_refs {
        let mut participants: BTreeSet<&str> = BTreeSet::new();
        participants.insert(raw.goal_call.endpoint.as_str());
        for flow in &endpoint_flows {
            for hop in &flow.hops {
                participants.insert(hop.src.as_str());
                participants.insert(hop.dst.as_str());
            }
        }
        for participant in participants {
            if let Some(entries) = refs.by_participant.get(participant) {
                for (test_id, _) in entries {
                    tests.insert((*test_id).to_owned());
                }
            }
        }
    }
    let tests: Vec<String> = tests.into_iter().collect();

    let writes: Vec<WriteFact> = raw.writes.into_iter().map(|(w, _)| w).collect();
    let goal = JourneyGoal {
        endpoint: raw.goal_call.endpoint.clone(),
        method: raw.goal_call.method.clone(),
        goal_basis: raw.goal_basis,
        write_coverage: match raw.goal_basis {
            GoalBasis::ObservedWrite => None,
            GoalBasis::MethodAssumed => Some("unknown".into()),
        },
        writes,
    };

    let step_screen_ids: Vec<&str> = steps.iter().map(|s| s.screen.as_str()).collect();
    let navigations_taken: Vec<(&str, &str, &str)> = steps
        .iter()
        .filter_map(|s| s.navigation.as_ref())
        .map(|n| (n.source.as_str(), n.target.as_str(), "navigates_to"))
        .collect();
    let id_digest = content_hash(&hash_bytes(&(
        JOURNEY_RULE_VERSION,
        &budget,
        &step_screen_ids,
        &navigations_taken,
        &goal.endpoint,
    )));
    let id = format!("journey:{repo}@{JOURNEY_RULE}:{}", &id_digest[..16]);

    // Hashed as stored, whichever form the base holds the `prov` in (#2215).
    let nav_edge_props: Vec<_> = nav_edge_props
        .iter()
        .map(|props| props.map(crate::workbench::stored_props))
        .collect();
    let support_fingerprint = if assumptions.is_empty() {
        content_hash(&hash_bytes(&(
            &steps,
            &nav_edge_props,
            &goal,
            &raw.earlier_assumed_goals,
            &result,
            &flow_triggers,
            &tests,
            &conditional_steps,
            &truncated,
            alternatives,
            alternatives_complete,
        )))
    } else {
        content_hash(&hash_bytes(&(
            &steps,
            &nav_edge_props,
            &goal,
            &raw.earlier_assumed_goals,
            &result,
            &flow_triggers,
            &tests,
            &conditional_steps,
            &truncated,
            alternatives,
            alternatives_complete,
            &assumptions,
        )))
    };

    let goal_screen_label = by_id
        .get(goal_screen_id.as_str())
        .map(|n| screen_display_label(n, renders_by_screen))
        .unwrap_or(goal_screen_id);
    let ep_path = by_id
        .get(goal.endpoint.as_str())
        .map(|ep| endpoint_path(ep, &goal.method))
        .unwrap_or(goal.endpoint.as_str());
    let label = match &result_step {
        Some(res) => {
            let res_label = by_id
                .get(res.screen.as_str())
                .map(|n| screen_display_label(n, renders_by_screen))
                .unwrap_or_else(|| res.screen.clone());
            format!(
                "{goal_screen_label} → {} {ep_path} → {res_label}",
                goal.method
            )
        }
        None => format!("{goal_screen_label} → {} {ep_path}", goal.method),
    };

    let confidence = weakest_confidence(supporting_confidences);
    let prov = Provenance::new(
        Tier::Semantic,
        confidence,
        citing_evidence,
        EXTRACTOR,
        &hash_bytes(&(&id, &support_fingerprint)),
    )
    .expect("InferredWeak/Gap is within the semantic tier ceiling")
    .with_method(ProducingMethod::Rule {
        rule: JOURNEY_RULE.into(),
        version: JOURNEY_RULE_VERSION.into(),
    });

    JourneyCandidate {
        id,
        support_fingerprint,
        entry: entry.id.clone(),
        steps,
        goal,
        earlier_assumed_goals: raw.earlier_assumed_goals,
        result,
        result_step,
        flows: flow_triggers,
        tests,
        conditional_steps,
        truncated,
        alternatives,
        alternatives_complete,
        alternative_of: None,
        label,
        assumptions,
        provenance: prov,
    }
}

/// Check whether the goal screen has a post-goal navigation with proven ordering.
///
/// Ordering is proven in `journey-candidate@1` (v1) only when:
/// - the goal's fetch site and a `NAVIGATES_TO` site (from `props.sites`) are in
///   the same source `Symbol` and the same file `path`,
/// - the fetch relation records `straight_line: true`, so no pass through
///   that Symbol's body reaches a later position without making the call
///   (a call under a branch, loop, `try` or callback, or a relation that
///   does not record this, proves nothing about a later navigation),
/// - the navigation site's `byte_start` is at or after the fetch's `byte_end`, and
/// - that site has `conditional == false`.
///
/// Or, whatever `straight_line` says (AC-1309): the site is after the fetch
/// in the same Symbol and file, and its `result_of` lists exactly the fetch's
/// byte range. The adapter records that only for a branch whose condition
/// reads a `const` bound to the call, so the call was made before the
/// branch ran (`const res = await callAPI(…); if (res.status === 201)`).
///
/// Any other conditional navigation after the call is `"unordered"`.
/// `"ordered"` says the navigation follows the call being made; it does not
/// say the call completed or succeeded first, since an unawaited call is
/// still made.
#[expect(clippy::too_many_arguments)]
fn find_ordered_result_step<'a>(
    goal_screen_id: &str,
    goal_call: &StepCall,
    path: &[PathNode<'a>],
    closures: &BTreeMap<&'a str, ScreenClosure<'a>>,
    by_id: &BTreeMap<&'a str, &'a Node>,
    nav_edge_props: &mut Vec<Option<&'a serde_json::Value>>,
    supporting_confidences: &mut Vec<ConfidenceTier>,
    citing_evidence: &mut Vec<EvidenceRef>,
    any_calls_truncated: &mut bool,
) -> (JourneyResult, Option<JourneyStep>, Option<NavAssumption>) {
    let Some(fetch_ev) = &goal_call.evidence else {
        return (JourneyResult::Unordered, None, None);
    };
    let straight_line = goal_call.straight_line == Some(true);
    let fetch_range = (fetch_ev.byte_start, fetch_ev.byte_end);
    let Some(closure) = closures.get(goal_screen_id) else {
        return (JourneyResult::Unordered, None, None);
    };

    let mut candidates: Vec<(u64, &OutgoingNav<'a>, Vec<NavigationSite>)> = Vec::new();
    for nav in &closure.outgoing_navs {
        if nav.source_symbol != goal_call.source {
            continue;
        }
        if path.iter().any(|p| p.screen == nav.target_screen) {
            continue;
        }
        let ordered_sites: Vec<NavigationSite> = nav
            .sites
            .iter()
            .filter(|site| {
                site.path == fetch_ev.path
                    && site.byte_start >= fetch_ev.byte_end
                    && ((straight_line && !site.conditional)
                        || site.result_of.contains(&fetch_range))
            })
            .cloned()
            .collect();
        if let Some(first) = ordered_sites.first() {
            candidates.push((first.byte_start, nav, ordered_sites));
        }
    }

    candidates.sort_by(|a, b| (a.0, &a.1.target_screen).cmp(&(b.0, &b.1.target_screen)));
    let Some((_, chosen_nav, ordered_sites)) = candidates.into_iter().next() else {
        return (JourneyResult::Unordered, None, None);
    };

    if let Some(screen_node) = by_id.get(chosen_nav.target_screen.as_str()) {
        supporting_confidences
            .push(provenance(&screen_node.props, &screen_node.id).confidence_tier);
    }
    supporting_confidences.push(edge_confidence(chosen_nav.edge));
    nav_edge_props.push(Some(&chosen_nav.edge.props));
    citing_evidence.extend(navigation_evidence(chosen_nav.edge, &ordered_sites));

    let res_closure = closures.get(chosen_nav.target_screen.as_str());
    if let Some(c) = res_closure
        && c.calls_truncated
    {
        *any_calls_truncated = true;
    }

    let step = JourneyStep {
        screen: chosen_nav.target_screen.clone(),
        navigation: Some(StepNavigation {
            source: chosen_nav.source_symbol.clone(),
            target: chosen_nav.target_screen.clone(),
            sites_taken: ordered_sites,
        }),
        calls: res_closure
            .map(|c| c.calls.iter().map(|(call, _)| call.clone()).collect())
            .unwrap_or_default(),
        unresolved: res_closure
            .map(|c| c.unresolved.clone())
            .unwrap_or_default(),
        unread_callbacks: res_closure
            .map(|c| c.unread_callbacks.clone())
            .unwrap_or_default(),
    };
    (
        JourneyResult::Ordered,
        Some(step),
        chosen_nav.assumption.clone(),
    )
}

fn navigation_evidence(edge: &Edge, sites_taken: &[NavigationSite]) -> Vec<EvidenceRef> {
    let prov = provenance(
        &edge.props,
        &format!("{} {} {}", edge.src, edge.label, edge.dst),
    );
    let default_repo = prov
        .evidence
        .first()
        .map(|e| e.repo.as_str())
        .unwrap_or_else(|| {
            edge.src
                .split_once(':')
                .and_then(|(_, r)| r.split_once('@'))
                .map_or("", |(repo, _)| repo)
        });
    let default_sha = prov
        .evidence
        .first()
        .map_or("unknown", |e| e.commit_sha.as_str());

    let mut out = Vec::new();
    for site in sites_taken {
        if let Some(matched) = prov.evidence.iter().find(|ev| {
            ev.path == site.path && ev.byte_start == site.byte_start && ev.byte_end == site.byte_end
        }) {
            out.push(matched.clone());
        } else {
            out.push(EvidenceRef {
                repo: default_repo.to_owned(),
                path: site.path.clone(),
                byte_start: site.byte_start,
                byte_end: site.byte_end,
                commit_sha: default_sha.to_owned(),
            });
        }
    }
    if out.is_empty() {
        out.extend(prov.evidence);
    }
    out
}

#[cfg(test)]
#[path = "journey_tests.rs"]
mod tests;
