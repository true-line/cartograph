//! Host-resolved evidence for agent handoff packets; no agent execution authority.

use ::work_packets::{self as packets, DecisionSelection, FactSelection};
use core_graph::source::{FactKey, FactSourceState};
use core_graph::{SnapshotReadLimits, SqliteGraphStore};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use tauri::Manager;

pub(crate) use app_store::packet_runtime::PacketRuntime;

#[derive(Debug, Serialize)]
pub(crate) struct PacketHostError {
    pub(crate) code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_review: Option<ReviewConflict>,
}
#[derive(Debug, Serialize)]
struct ReviewConflict {
    revision: u64,
    status: String,
}
impl PacketHostError {
    fn code(code: &'static str) -> Self {
        Self {
            code,
            current_review: None,
        }
    }
    fn storage() -> Self {
        Self {
            code: "storage_failure",
            current_review: None,
        }
    }
}
impl From<::work_packets::PacketError> for PacketHostError {
    fn from(error: ::work_packets::PacketError) -> Self {
        use ::work_packets::PacketError::*;
        let code = match error {
            InvalidInput(_) => "invalid_input",
            LimitExceeded(_) => "limit_exceeded",
            SelectionConflict(_) => "selection_conflict",
            NotFound => "unavailable_evidence",
            Storage(_) => "storage_failure",
            ReviewConflict { revision, status } => {
                return Self {
                    code: "review_conflict",
                    current_review: Some(self::ReviewConflict { revision, status }),
                };
            }
        };
        Self {
            code,
            current_review: None,
        }
    }
}
async fn packet_worker<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, PacketHostError> + Send + 'static,
) -> Result<T, PacketHostError> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|_| PacketHostError::storage())?
}
fn timestamp() -> Result<String, PacketHostError> {
    Ok(format!(
        "unix-ms:{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| PacketHostError::storage())?
            .as_millis()
    ))
}

#[tauri::command]
pub(crate) async fn list_work_packets(
    limit: usize,
    after: Option<String>,
    app: tauri::AppHandle,
) -> Result<Vec<::work_packets::PacketSummary>, PacketHostError> {
    packet_worker(move || {
        let state = app.state::<crate::AppState>();
        let store = state
            .work_packets
            .store
            .lock()
            .map_err(|_| PacketHostError::storage())?;
        store
            .list_after(limit, after.as_deref())
            .map_err(Into::into)
    })
    .await
}

#[tauri::command]
pub(crate) async fn get_work_packet(
    packet_id: String,
    app: tauri::AppHandle,
) -> Result<::work_packets::PacketDetail, PacketHostError> {
    packet_worker(move || {
        let state = app.state::<crate::AppState>();
        let store = state
            .work_packets
            .store
            .lock()
            .map_err(|_| PacketHostError::storage())?;
        store.get(&packet_id).map_err(Into::into)
    })
    .await
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewRequest {
    packet_id: String,
    expected_revision: u64,
    decision: ::work_packets::ReviewDecision,
    note: Option<String>,
}
#[tauri::command]
pub(crate) async fn review_work_packet(
    request: ReviewRequest,
    app: tauri::AppHandle,
) -> Result<::work_packets::PacketDetail, PacketHostError> {
    packet_worker(move || {
        let state = app.state::<crate::AppState>();
        let mut store = state
            .work_packets
            .store
            .lock()
            .map_err(|_| PacketHostError::storage())?;
        store
            .review(
                &request.packet_id,
                request.expected_revision,
                request.decision,
                request.note,
                &timestamp()?,
            )
            .map_err(Into::into)
    })
    .await
}

#[derive(Debug, Serialize)]
pub(crate) struct PacketExport {
    packet_id: String,
    file_name: String,
    content: String,
}
#[tauri::command]
pub(crate) async fn export_work_packet(
    packet_id: String,
    app: tauri::AppHandle,
) -> Result<PacketExport, PacketHostError> {
    packet_worker(move || {
        let state = app.state::<crate::AppState>();
        let detail = state
            .work_packets
            .store
            .lock()
            .map_err(|_| PacketHostError::storage())?
            .get(&packet_id)?;
        let content = ::work_packets::export_markdown(&detail)?;
        let suffix = packet_id
            .strip_prefix("work-packet-v1:")
            .ok_or_else(PacketHostError::storage)?;
        Ok(PacketExport {
            file_name: format!("cartograph-work-packet-{suffix}.md"),
            packet_id,
            content,
        })
    })
    .await
}

#[derive(Debug, Serialize)]
pub(crate) struct PreparedSelection {
    pub(crate) snapshot_id: String,
    pub(crate) facts: Vec<FactSelection>,
    pub(crate) interpretation_decisions: Vec<DecisionSelection>,
}

fn snapshot_limits() -> SnapshotReadLimits {
    SnapshotReadLimits {
        max_bytes: SnapshotReadLimits::MAX_BYTES,
        max_rows: SnapshotReadLimits::MAX_ROWS,
        max_depth: SnapshotReadLimits::MAX_DEPTH,
        max_values: SnapshotReadLimits::MAX_VALUES,
    }
}

fn admit_serialized(value: &impl Serialize, maximum: usize) -> Result<(), String> {
    struct Counter {
        remaining: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.remaining = self
                .remaining
                .checked_sub(bytes.len())
                .ok_or_else(|| std::io::Error::other("packet byte limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter { remaining: maximum }, value)
        .map_err(|_| "limit_exceeded".into())
}

pub(crate) fn prepare_selection(
    graph: &SqliteGraphStore,
    decisions: &agents::DecisionLog,
    facts: &[FactKey],
    interpretation_decisions: &[String],
) -> Result<PreparedSelection, String> {
    if facts.len() > 64 || interpretation_decisions.len() > 64 {
        return Err("limit_exceeded".into());
    }
    admit_serialized(&(facts, interpretation_decisions), 128 * 1024)?;
    let observation = graph
        .read_source_selection_snapshot_bounded(facts, snapshot_limits())
        .map_err(|e| graph_error(e).code.to_string())?;
    let snapshot = context_hub::ContextSnapshot::new(observation.graph.0, observation.graph.1)
        .map_err(|_| "unavailable_evidence".to_string())?;
    let facts = observation
        .selections
        .into_iter()
        .map(|selected| {
            let expected_digest = match selected.state {
                FactSourceState::Present { fact_digest, .. }
                | FactSourceState::Absent { fact_digest }
                | FactSourceState::Invalid {
                    fact_digest: Some(fact_digest),
                } => fact_digest,
                _ => return Err("selection_conflict".to_string()),
            };
            Ok(FactSelection {
                reference: selected.fact,
                expected_digest,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let records = decisions
        .read_selected_assertions(interpretation_decisions)
        .map_err(|_| "unavailable_evidence".to_string())?;
    let interpretation_decisions = records
        .into_iter()
        .map(|selected| {
            let record = selected
                .record
                .ok_or_else(|| "selection_conflict".to_string())?;
            let expected_record_hash = ::work_packets::decision_record_hash(&record)
                .map_err(|_| "unavailable_evidence".to_string())?;
            Ok(DecisionSelection {
                content_hash: selected.content_hash,
                expected_record_hash,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let result = PreparedSelection {
        snapshot_id: snapshot.id().into(),
        facts,
        interpretation_decisions,
    };
    admit_serialized(&result, 128 * 1024)?;
    Ok(result)
}

/// Resolve tray references against one bounded graph/association observation.
/// Subsequent generation must compare this exact snapshot and content again.
#[tauri::command]
pub(crate) async fn prepare_work_packet_selection(
    facts: Vec<FactKey>,
    interpretation_decisions: Vec<String>,
    app: tauri::AppHandle,
) -> Result<PreparedSelection, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<crate::AppState>();
        let graph = state
            .graph
            .lock()
            .map_err(|_| "storage_failure".to_string())?;
        let decisions = state
            .decisions
            .lock()
            .map_err(|_| "storage_failure".to_string())?;
        prepare_selection(&graph, &decisions, &facts, &interpretation_decisions)
    })
    .await
}

// Raw JSON is essential: Tauri's generic object decoder cannot preserve duplicate
// protocol keys for the generation boundary's preflight.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationRequest {
    expected_snapshot_id: String,
    #[serde(default)]
    facts: Vec<packets::FactSelection>,
    #[serde(default)]
    artifacts: Vec<packets::ArtifactSelection>,
    #[serde(default)]
    interpretation_decisions: Vec<packets::DecisionSelection>,
    #[serde(default)]
    intent: packets::PacketIntent,
    #[serde(default)]
    work: packets::PacketWork,
    #[serde(default)]
    acceptance: packets::PacketAcceptance,
    #[serde(default)]
    handoff: packets::PacketHandoff,
    #[serde(default, rename = "return")]
    returns: packets::PacketReturn,
    #[serde(default)]
    supersedes: Vec<String>,
}
fn parse_generation(
    json: &str,
) -> Result<(GenerationRequest, packets::EvidenceSelection), PacketHostError> {
    let request: GenerationRequest = packets::deserialize_json(
        json,
        packets::JsonInputLimits::GENERATION_REQUEST,
    )
    .map_err(|error| match error {
        packets::JsonInputError::LimitExceeded { .. } => PacketHostError::code("limit_exceeded"),
        _ => PacketHostError::code("invalid_input"),
    })?;
    let selection = packets::EvidenceSelection {
        expected_snapshot_id: request.expected_snapshot_id.clone(),
        facts: request.facts.clone(),
        artifacts: request.artifacts.clone(),
        interpretation_decisions: request.interpretation_decisions.clone(),
    }
    .normalize()
    .map_err(|error| {
        PacketHostError::code(match error {
            packets::SelectionError::InvalidInput => "invalid_input",
            packets::SelectionError::LimitExceeded => "limit_exceeded",
            packets::SelectionError::SelectionConflict => "selection_conflict",
        })
    })?;
    const IDS: &[&str] = &[
        "user-stories",
        "us-tm",
        "us-tm-index",
        "flow-dossiers",
        "topology",
        "data-model",
        "adrs",
        "gap-register",
        "gap-register-index",
        "drift-register",
        "security-view",
        "toolchain",
        "test-arrangements",
        "journeys",
        "rule-evidence",
        "rule-evidence-index",
        "endpoint-permissions",
        "configuration",
        // `external` is refused until a decision says otherwise (SPEC-13).
        // `environments` is refused until a decision says otherwise (SPEC-14).
        // `documentation-evidence` is refused until a decision says otherwise
        // (SPEC-18).
    ];
    if selection
        .artifacts
        .iter()
        .any(|a| !IDS.contains(&a.id.as_str()))
    {
        return Err(PacketHostError::code("invalid_input"));
    }
    // User knowledge is retained as requested intent. It cannot impersonate a
    // host observation or supply a review/execution/coverage evidence record.
    let groups = serde_json::to_value((
        &request.intent,
        &request.work,
        &request.acceptance,
        &request.handoff,
        &request.returns,
    ))
    .map_err(|_| PacketHostError::code("invalid_input"))?;
    fn requested(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(object) => {
                object.get("origin").is_none_or(|o| {
                    o.get("kind").and_then(|k| k.as_str()) == Some("requested_intent")
                }) && object.values().all(requested)
            }
            serde_json::Value::Array(a) => a.iter().all(requested),
            _ => true,
        }
    }
    if !requested(&groups)
        || [
            &request.acceptance.discovered_tests,
            &request.acceptance.structural_test_associations,
            &request.acceptance.execution_evidence,
            &request.acceptance.coverage_evidence,
            &request.acceptance.assertion_support,
        ]
        .iter()
        .any(|a| !a.is_empty())
    {
        return Err(PacketHostError::code("invalid_input"));
    }
    if request.supersedes.len() > 1024 {
        return Err(PacketHostError::code("limit_exceeded"));
    }
    Ok((request, selection))
}
fn source_error(_: impl std::fmt::Debug) -> PacketHostError {
    PacketHostError::code("unavailable_evidence")
}
fn conflict() -> PacketHostError {
    PacketHostError::code("selection_conflict")
}
fn decision_error(error: agents::DecisionSelectionError) -> PacketHostError {
    PacketHostError::code(match error {
        agents::DecisionSelectionError::InvalidSelection => "invalid_input",
        agents::DecisionSelectionError::LimitExceeded => "limit_exceeded",
        _ => "unavailable_evidence",
    })
}
fn graph_error(error: core_graph::GraphError) -> PacketHostError {
    use core_graph::{GraphError as E, SnapshotBoundsError as B};
    PacketHostError::code(match error {
        E::Storage(_) => "storage_failure",
        E::SnapshotBounds(
            B::RowLimit
            | B::RawByteLimit
            | B::CanonicalByteLimit
            | B::JsonDepthLimit
            | B::JsonValueLimit,
        ) => "limit_exceeded",
        _ => "unavailable_evidence",
    })
}
fn observed(
    state: &crate::AppState,
    keys: &[FactKey],
) -> Result<core_graph::source::SourceSelectionSnapshot, PacketHostError> {
    state
        .graph
        .lock()
        .map_err(|_| PacketHostError::storage())?
        .read_source_selection_snapshot_bounded(keys, snapshot_limits())
        .map_err(graph_error)
}
fn snapshot_id(
    observation: &core_graph::source::SourceSelectionSnapshot,
) -> Result<String, PacketHostError> {
    context_hub::ContextSnapshot::new(observation.graph.0.clone(), observation.graph.1.clone())
        .map(|s| s.id().into())
        .map_err(source_error)
}
fn curation(
    state: &crate::AppState,
    keys: &[String],
    artifacts: bool,
) -> Result<agents::CurationObservation, PacketHostError> {
    let decisions = state
        .decisions
        .lock()
        .map_err(|_| PacketHostError::storage())?;
    if artifacts {
        decisions
            .read_curation_observation(keys)
            .map_err(decision_error)
    } else {
        Ok(agents::CurationObservation {
            rejected_hashes: BTreeSet::new(),
            selected: decisions
                .read_selected_assertions(keys)
                .map_err(decision_error)?,
        })
    }
}
fn supported_provenance(props: &serde_json::Value) -> Option<core_prov::Provenance> {
    props
        .get("prov")
        .cloned()
        .and_then(|v| serde_json::from_value::<core_prov::Provenance>(v).ok())
        .filter(|p| p.validate().is_ok())
}
fn supplied<T>(value: T, ids: Vec<String>) -> packets::Known<T> {
    packets::Known::Supplied {
        value,
        origin: packets::Origin::Evidence { evidence_ids: ids },
    }
}
fn serialized_len(value: &impl Serialize, maximum: usize) -> Result<usize, PacketHostError> {
    struct Count {
        used: usize,
        maximum: usize,
    }
    impl std::io::Write for Count {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.used = self
                .used
                .checked_add(b.len())
                .filter(|n| *n <= self.maximum)
                .ok_or_else(|| std::io::Error::other("packet byte limit"))?;
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count { used: 0, maximum };
    serde_json::to_writer(&mut count, value)
        .map_err(|_| PacketHostError::code("limit_exceeded"))?;
    Ok(count.used)
}
#[derive(Default)]
struct Limitations(BTreeMap<&'static str, (&'static str, BTreeSet<String>)>);
impl Limitations {
    fn add(&mut self, code: &'static str, detail: &'static str, pointer: impl Into<String>) {
        self.0
            .entry(code)
            .or_insert_with(|| (detail, BTreeSet::new()))
            .1
            .insert(pointer.into());
    }
    fn finish(self) -> Result<Vec<packets::EvidenceLimitation>, PacketHostError> {
        self.0
            .into_iter()
            .map(|(code, (detail, pointers))| {
                let pointers: Vec<_> = pointers.into_iter().collect();
                Ok(packets::EvidenceLimitation {
                    id: packets::evidence_id("limitation", &(code, &pointers))?,
                    pointers,
                    omission: packets::missing_omission(detail),
                })
            })
            .collect()
    }
}
fn selected_fact(
    graph: &(Vec<core_graph::Node>, Vec<core_graph::Edge>),
    key: &FactKey,
) -> Option<packets::PacketFact> {
    match key {
        FactKey::Node { id } => graph
            .0
            .iter()
            .find(|n| &n.id == id)
            .cloned()
            .map(packets::PacketFact::Node),
        FactKey::Edge {
            source,
            label,
            destination,
        } => graph
            .1
            .iter()
            .find(|e| &e.src == source && &e.label == label && &e.dst == destination)
            .cloned()
            .map(packets::PacketFact::Edge),
    }
}
fn admit_fact(
    graph: &(Vec<core_graph::Node>, Vec<core_graph::Edge>),
    key: &FactKey,
    remaining: usize,
) -> Result<usize, PacketHostError> {
    match key {
        FactKey::Node { id } => graph
            .0
            .iter()
            .find(|n| &n.id == id)
            .map(|n| serialized_len(n, remaining)),
        FactKey::Edge {
            source,
            label,
            destination,
        } => graph
            .1
            .iter()
            .find(|e| &e.src == source && &e.label == label && &e.dst == destination)
            .map(|e| serialized_len(e, remaining)),
    }
    .ok_or_else(conflict)?
}
fn binding(state: &FactSourceState) -> Option<&core_graph::source::SourceBinding> {
    if let FactSourceState::Present { binding, .. } = state {
        Some(binding)
    } else {
        None
    }
}
fn state_digest(state: &FactSourceState) -> Option<&str> {
    match state {
        FactSourceState::Present { fact_digest, .. }
        | FactSourceState::Absent { fact_digest }
        | FactSourceState::Invalid {
            fact_digest: Some(fact_digest),
        } => Some(fact_digest),
        _ => None,
    }
}

// Exact legacy subject equality is considered only against all typed graph
// identities. Never split concatenated edge ids or silently choose a collision.
fn decision_relationship(
    record: &agents::AssertionDecisionRecord,
    graph: &(Vec<core_graph::Node>, Vec<core_graph::Edge>),
    selected: &[packets::FactEvidence],
) -> packets::Known<packets::FactRelationship> {
    let subject = &record.assertion.subject_id;
    let mut unique = None;
    let mut ambiguous = false;
    for n in &graph.0 {
        if &n.id == subject && unique.replace(FactKey::from_node(n)).is_some() {
            ambiguous = true;
            break;
        }
    }
    if !ambiguous {
        for e in &graph.1 {
            // Compare the native concatenation using the already-typed endpoints;
            // this never decodes a legacy string into a graph key.
            let equal = subject
                .strip_prefix(e.src.as_str())
                .and_then(|s| s.strip_prefix(' '))
                .and_then(|s| s.strip_prefix(e.label.as_str()))
                .and_then(|s| s.strip_prefix(' '))
                == Some(e.dst.as_str());
            if equal && unique.replace(FactKey::from_edge(e)).is_some() {
                ambiguous = true;
                break;
            }
        }
    }
    if !ambiguous
        && let Some(reference) = unique
        && let Some(fact) = selected.iter().find(|f| f.reference == reference)
        && let Some(current_provenance) = &fact.provenance
    {
        return supplied(
            packets::FactRelationship {
                reference,
                stale: current_provenance.content_hash != record.assertion.provenance.content_hash,
            },
            vec![fact.id.clone()],
        );
    }
    packets::Known::Missing {
        omission: core_graph::omission::Omission {
            class: core_graph::omission::OmissionClass::UnresolvedRelationship,
            seam: core_graph::omission::FillSeam::Interpretation,
            detail: "Historical decision has no unique selected typed fact association".into(),
        },
    }
}

pub(crate) fn generate(
    state: &crate::AppState,
    runtime: &PacketRuntime,
    json: &str,
) -> Result<packets::PacketDetail, PacketHostError> {
    let (request, selection) = parse_generation(json)?;
    let keys: Vec<_> = selection
        .facts
        .iter()
        .map(|s| s.reference.clone())
        .collect();
    let preliminary = observed(state, &keys)?;
    if snapshot_id(&preliminary)? != selection.expected_snapshot_id {
        return Err(conflict());
    }
    for (selected, actual) in selection.facts.iter().zip(&preliminary.selections) {
        if actual.fact != selected.reference
            || state_digest(&actual.state) != Some(selected.expected_digest.as_str())
        {
            return Err(conflict());
        }
    }
    let mut sources = BTreeMap::new();
    for selected in &preliminary.selections {
        if let Some(binding) = binding(&selected.state)
            && !sources.contains_key(&binding.repo_key)
        {
            if sources.len() >= 16 {
                return Err(PacketHostError::code("limit_exceeded"));
            }
            let source = state
                .sources
                .lock()
                .map_err(|_| PacketHostError::storage())?
                .get_by_repo(&binding.repo_key)
                .map_err(source_error)?;
            sources.insert(binding.repo_key.clone(), source);
        }
    }
    let guards: BTreeMap<_, _> = sources
        .values()
        .flatten()
        .map(|s| {
            (
                s.source_id.clone(),
                state.primary_sources.task_guard(&s.source_id),
            )
        })
        .collect();
    let observation = observed(state, &keys)?;
    if observation != preliminary {
        return Err(conflict());
    }
    let mut body = packets::PacketBody::draft(selection.expected_snapshot_id.clone());
    body.intent = request.intent;
    body.work = request.work;
    body.acceptance = request.acceptance;
    body.handoff = request.handoff;
    body.returns = request.returns;
    let mut limitations = Limitations::default();
    limitations.add(
        "closure",
        "Producer input, membership and configuration closure are not established",
        "identity.input_closure",
    );
    limitations.add(
        "curated",
        "H3: curated context is unavailable in this product slice",
        "identity.curated_context",
    );
    let mut fact_bytes = 0;
    for (selected, actual) in selection.facts.iter().zip(&observation.selections) {
        fact_bytes += admit_fact(
            &observation.graph,
            &selected.reference,
            packets::MAX_BODY_BYTES - fact_bytes,
        )?;
        let fact = selected_fact(&observation.graph, &selected.reference).ok_or_else(conflict)?;
        let provenance = supported_provenance(fact.props());
        let id = packets::evidence_id("fact", &(&selected.reference, &selected.expected_digest))?;
        let source_association = match binding(&actual.state) {
            Some(b) => supplied(b.clone(), vec![id.clone()]),
            None => packets::Known::missing(
                "No valid captured source association is available for this exact fact",
            ),
        };
        body.evidence.facts.push(packets::FactEvidence {
            id,
            reference: selected.reference.clone(),
            expected_digest: selected.expected_digest.clone(),
            fact,
            provenance_problem: provenance.is_none().then(|| {
                "Stored provenance is missing or invalid; no confirmed provenance was inferred"
                    .into()
            }),
            provenance,
            source_association,
            review_annotations: vec![],
        });
    }
    let mut decision_keys: BTreeSet<String> = selection
        .interpretation_decisions
        .iter()
        .map(|d| d.content_hash.clone())
        .collect();
    for fact in &body.evidence.facts {
        if let Some(p) = &fact.provenance
            && matches!(
                p.confidence_tier,
                core_prov::ConfidenceTier::InferredStrong | core_prov::ConfidenceTier::InferredWeak
            )
        {
            decision_keys.insert(p.content_hash.clone());
        }
    }
    if decision_keys.len() > 64 {
        return Err(PacketHostError::code("limit_exceeded"));
    }
    let decision_keys: Vec<_> = decision_keys.into_iter().collect();
    let policy = curation(state, &decision_keys, !selection.artifacts.is_empty())?;
    for expected in &selection.interpretation_decisions {
        let actual = policy
            .selected
            .iter()
            .find(|d| d.content_hash == expected.content_hash)
            .and_then(|d| d.record.as_ref())
            .ok_or_else(conflict)?;
        if packets::decision_record_hash(actual)? != expected.expected_record_hash {
            return Err(conflict());
        }
    }
    for item in &policy.selected {
        let Some(record) = &item.record else {
            continue;
        };
        let record_hash = packets::decision_record_hash(record)?;
        let relationship = decision_relationship(record, &observation.graph, &body.evidence.facts);
        let annotation = packets::ReviewAnnotation {
            id: packets::evidence_id("decision", &(&item.content_hash, &record_hash))?,
            record_hash: record_hash.clone(),
            record: record.clone(),
            relationship,
        };
        if let Some(r) = annotation.relationship.value()
            && let Some(fact) = body
                .evidence
                .facts
                .iter_mut()
                .find(|f| f.reference == r.reference)
        {
            fact.review_annotations.push(annotation);
        } else {
            let id = packets::evidence_id("interpretation", &record_hash)?;
            let review = serde_json::to_value(record.decision)
                .map_err(source_error)?
                .as_str()
                .ok_or_else(|| PacketHostError::code("unavailable_evidence"))?
                .to_owned();
            body.evidence.interpretations.push(packets::InterpretationEvidence{id:id.clone(),text:record.assertion.summary.clone(),origin:packets::Origin::Evidence{evidence_ids:vec![id.clone()]},supporting_evidence_ids:vec![],review:supplied(review,vec![id]),freshness:packets::Known::missing("Historical decision has no verified relationship to the selected current fact"),conflict:packets::Known::missing("No conflict resolution was supplied"),historical_decision:Some(annotation)});
        }
    }
    let mut validation_bytes = 0u64;
    let mut text_bytes = 0usize;
    for fact in &body.evidence.facts {
        let Some(binding) = fact.source_association.value() else {
            continue;
        };
        let Some(source) = sources.get(&binding.repo_key).and_then(Option::as_ref) else {
            limitations.add(
                "registration",
                "Bound source registration is unavailable",
                fact.id.clone(),
            );
            continue;
        };
        let Some(Ok(guard)) = guards.get(&source.source_id) else {
            limitations.add(
                "retained",
                "Pinned historical source could not be acquired; no current text was substituted",
                fact.id.clone(),
            );
            continue;
        };
        let receipt = match state.primary_sources.task_receipt(
            guard,
            &fact.reference,
            &fact.expected_digest,
            &binding.repo_key,
            &binding.receipt_id,
        ) {
            Ok(r) => r,
            Err(_) => {
                limitations.add("retained","Pinned historical source is unavailable or invalid; no current text was substituted",fact.id.clone());
                continue;
            }
        };
        for (inventory_index, range) in receipt.ranges().iter().enumerate() {
            let span_bytes = range
                .captured
                .byte_end
                .saturating_sub(range.captured.byte_start) as usize;
            if body.evidence.source_excerpts.len() >= 12
                || span_bytes > 8192
                || span_bytes > 48 * 1024 - text_bytes
                || range.captured.file.byte_len > 128 * 1024 * 1024 - validation_bytes
            {
                limitations.add("excerpt_limit","Additional pinned source ranges exceed the excerpt or complete-file validation budget",fact.id.clone());
                continue;
            }
            validation_bytes += range.captured.file.byte_len;
            let text = match state
                .primary_sources
                .task_text(guard, &receipt, inventory_index)
            {
                Ok(t) => t,
                Err(_) => {
                    limitations.add("retained","Pinned historical source is unavailable or invalid; no current text was substituted",fact.id.clone());
                    continue;
                }
            };
            if text.len() != span_bytes {
                return Err(conflict());
            }
            text_bytes += text.len();
            let role = serde_json::to_value(range.role)
                .map_err(source_error)?
                .as_str()
                .ok_or_else(|| PacketHostError::code("unavailable_evidence"))?
                .to_owned();
            let index = range.index as usize;
            let id = packets::evidence_id(
                "excerpt",
                &(
                    &fact.reference,
                    &binding.receipt_id,
                    &role,
                    index,
                    &range.captured,
                    &text,
                ),
            )?;
            body.evidence.source_excerpts.push(packets::SourceExcerpt {
                id,
                fact: fact.reference.clone(),
                receipt_id: binding.receipt_id.clone(),
                range_role: role,
                range_index: index,
                span: range.captured.clone(),
                text,
            });
        }
    }
    for source in sources.values().flatten() {
        body.identity.sources.push(packets::SourceIdentity{registration_id:source.source_id.clone(),repo_key:source.repo_key.clone(),revision:packets::Known::missing("A complete current registration/revision binding is not established by primary-file receipts")});
    }
    let flows = if selection.artifacts.is_empty() {
        None
    } else {
        flowtracer::trace_bounded(
            &observation.graph.0,
            &observation.graph.1,
            flowtracer::TraceLimits {
                max_flows: 4096,
                max_hops: 16384,
                max_output_bytes: 8 * 1024 * 1024,
            },
        )
        .ok()
    };
    let mut artifact_remaining = packets::MAX_ARTIFACT_BYTES;
    for selected in &selection.artifacts {
        let artifact = if let Some(flows) = &flows {
            match spec::compile_spec_selected(
                &observation.graph.0,
                &observation.graph.1,
                flows,
                selected.mode,
                &policy.rejected_hashes,
                &[&selected.id],
                spec::SelectedCompileLimits {
                    max_bytes: artifact_remaining,
                    adr: spec::AdrLimits {
                        max_rows: 50_000,
                        max_output_bytes: 32 * 1024 * 1024,
                    },
                },
            )
            .map_err(|_| PacketHostError::code("invalid_input"))?
            .pop()
            {
                Some(spec::SelectedArtifactOutcome::Produced(artifact)) => {
                    if !artifact.verify_identity()
                        || artifact.id != selected.id
                        || artifact.schema != selected.schema
                        || artifact.content_id != selected.content_id
                    {
                        return Err(conflict());
                    }
                    let used = artifact.content.len()
                        + serialized_len(
                            &artifact.assertions,
                            artifact_remaining.saturating_sub(artifact.content.len()),
                        )?;
                    artifact_remaining = artifact_remaining
                        .checked_sub(used)
                        .ok_or_else(|| PacketHostError::code("limit_exceeded"))?;
                    supplied(artifact, vec![packets::evidence_id("artifact", selected)?])
                }
                _ => packets::Known::missing(
                    "Artifact construction exceeded its bounded output or shared derivation budget; requested identity remains unverified",
                ),
            }
        } else {
            packets::Known::missing(
                "Artifact construction exceeded the bounded tracing budget; requested identity remains unverified",
            )
        };
        body.evidence.artifacts.push(packets::ArtifactEvidence {
            id: packets::evidence_id("artifact", selected)?,
            selection: selected.clone(),
            artifact,
        });
    }
    body.evidence.limitations = limitations.finish()?;
    body.normalize()?;
    if observed(state, &keys)? != observation
        || curation(state, &decision_keys, !selection.artifacts.is_empty())? != policy
    {
        return Err(conflict());
    }
    for (repo, expected) in &sources {
        let current = state
            .sources
            .lock()
            .map_err(|_| PacketHostError::storage())?
            .get_by_repo(repo)
            .map_err(source_error)?;
        let identity = |source: &crate::sources::RegisteredSource| {
            (
                source.source_id.clone(),
                source.repo_key.clone(),
                source.root().to_owned(),
            )
        };
        if current.as_ref().map(identity) != expected.as_ref().map(identity) {
            return Err(conflict());
        }
    }
    // `guards` holds each cited source's shared `task_guard` until after `put`
    // commits, so a packet written under that guard cannot interleave with an
    // exclusive forget. A guard that could not be taken (a forget is running)
    // would let `put` index receipts that forget is about to reclaim, so the
    // save is refused and the caller retries (ADR-0046 shared predicate).
    if guards.values().any(Result::is_err) {
        return Err(conflict());
    }
    let detail = runtime
        .store
        .lock()
        .map_err(|_| PacketHostError::storage())?
        .put(body, &request.supersedes, &timestamp()?)
        .map_err(Into::into);
    drop(guards);
    detail
}
#[tauri::command]
pub(crate) async fn generate_work_packet(
    request_json: String,
    app: tauri::AppHandle,
) -> Result<packets::PacketDetail, PacketHostError> {
    packet_worker(move || {
        let state = app.state::<crate::AppState>();
        generate(&state, &state.work_packets, &request_json)
    })
    .await
}

fn assess(
    state: &crate::AppState,
    runtime: &PacketRuntime,
    id: &str,
) -> Result<packets::PacketDetail, PacketHostError> {
    use packets::{FreshnessInput as I, FreshnessObservation as O, FreshnessResult as R};
    let saved = runtime
        .store
        .lock()
        .map_err(|_| PacketHostError::storage())?
        .get(id)?;
    let keys: Vec<_> = saved
        .body
        .evidence
        .facts
        .iter()
        .map(|f| f.reference.clone())
        .collect();
    let mut checks = vec![];
    let mut limitations=vec!["Configuration/dependency closure and complete source membership are unsupported; assessment cannot establish full currentness".into(),"Working-tree acquisition observes bounded bytes and is not an atomic filesystem snapshot".into()];
    let before = observed(state, &keys);
    let mut observed_snapshot = None;
    if let Ok(observation) = &before {
        let snapshot = snapshot_id(observation)?;
        checks.push(O {
            input: I::Graph,
            subject: "recovered_graph".into(),
            result: if snapshot == saved.body.identity.recovered_basis.snapshot_id {
                R::Equal
            } else {
                R::Changed
            },
        });
        observed_snapshot = Some(snapshot);
        for fact in &saved.body.evidence.facts {
            let actual = observation
                .selections
                .iter()
                .find(|s| s.fact == fact.reference);
            let equal = actual.is_some_and(|s| {
                state_digest(&s.state) == Some(fact.expected_digest.as_str())
                    && binding(&s.state) == fact.source_association.value()
            });
            checks.push(O {
                input: I::Associations,
                subject: fact.id.clone(),
                result: if !equal {
                    R::Changed
                } else if fact.source_association.value().is_none() {
                    R::Unsupported
                } else {
                    R::Equal
                },
            });
        }
    } else {
        checks.push(O {
            input: I::Graph,
            subject: "recovered_graph".into(),
            result: R::Unavailable,
        });
        checks.push(O {
            input: I::Associations,
            subject: "selected_facts".into(),
            result: R::Unavailable,
        });
    }
    if saved.body.evidence.facts.is_empty() {
        checks.push(O {
            input: I::Associations,
            subject: "artifact_input_associations".into(),
            result: R::Unsupported,
        });
    }
    for registered in &saved.body.identity.sources {
        let actual = state
            .sources
            .lock()
            .map_err(|_| PacketHostError::storage())?
            .get_by_id(&registered.registration_id)
            .map_err(source_error)?;
        checks.push(O {
            input: I::RegistrationRevision,
            subject: registered.registration_id.clone(),
            result: match actual {
                Some(source) if source.repo_key == registered.repo_key => R::Unsupported,
                Some(_) => R::Changed,
                None => R::Unavailable,
            },
        });
    }
    checks.push(O {
        input: I::ConfigurationClosure,
        subject: "producer_input_closure".into(),
        result: R::Unsupported,
    });
    checks.push(O {
        input: I::Membership,
        subject: "source_membership_and_ignore_inputs".into(),
        result: R::Unsupported,
    });
    let mut charged = 0u64;
    let mut files = BTreeSet::new();
    for excerpt in &saved.body.evidence.source_excerpts {
        let file = &excerpt.span.file;
        let subject = packets::evidence_id("file", file)?;
        let source = state
            .sources
            .lock()
            .map_err(|_| PacketHostError::storage())?
            .get_by_id(file.source_id.as_str())
            .map_err(source_error)?;
        let Some(source) = source else {
            checks.push(O {
                input: I::RegistrationRevision,
                subject: subject.clone(),
                result: R::Unavailable,
            });
            checks.push(O {
                input: I::RetainedIntegrity,
                subject: excerpt.id.clone(),
                result: R::Unavailable,
            });
            checks.push(O {
                input: I::LiveFileBytes,
                subject,
                result: R::Unavailable,
            });
            continue;
        };
        let fact = saved
            .body
            .evidence
            .facts
            .iter()
            .find(|f| f.reference == excerpt.fact)
            .ok_or_else(|| PacketHostError::code("unavailable_evidence"))?;
        let Some(binding) = fact.source_association.value() else {
            return Err(PacketHostError::code("unavailable_evidence"));
        };
        let registration_equal =
            source.repo_key == binding.repo_key
                && saved.body.identity.sources.iter().any(|s| {
                    s.registration_id == source.source_id && s.repo_key == source.repo_key
                });
        checks.push(O {
            input: I::RegistrationRevision,
            subject: subject.clone(),
            result: if registration_equal {
                R::Unsupported
            } else {
                R::Changed
            },
        });
        let retained = (|| {
            let guard = state.primary_sources.task_guard(&source.source_id).ok()?;
            let receipt = state
                .primary_sources
                .task_receipt(
                    &guard,
                    &fact.reference,
                    &fact.expected_digest,
                    &binding.repo_key,
                    &excerpt.receipt_id,
                )
                .ok()?;
            let (index, range) = receipt.ranges().iter().enumerate().find(|(_, r)| {
                r.index as usize == excerpt.range_index
                    && serde_json::to_value(r.role)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .as_deref()
                        == Some(excerpt.range_role.as_str())
                    && r.captured == excerpt.span
            })?;
            if range.captured.file.byte_len > 128 * 1024 * 1024 - charged {
                return None;
            }
            charged += range.captured.file.byte_len;
            state
                .primary_sources
                .task_text(&guard, &receipt, index)
                .ok()
                .map(|text| text == excerpt.text)
        })();
        checks.push(O {
            input: I::RetainedIntegrity,
            subject: excerpt.id.clone(),
            result: match retained {
                Some(true) => R::Equal,
                Some(false) => R::Changed,
                None => R::Unavailable,
            },
        });
        if retained != Some(true) {
            checks.push(O {
                input: I::LiveFileBytes,
                subject,
                result: R::Unavailable,
            });
            continue;
        }
        if !files.insert((
            file.source_id.as_str().to_owned(),
            file.path.clone(),
            file.capture_id.clone(),
            file.digest.clone(),
            file.byte_len,
        )) {
            continue;
        }
        if !registration_equal || file.byte_len > 128 * 1024 * 1024 - charged {
            checks.push(O {
                input: I::LiveFileBytes,
                subject,
                result: R::Unavailable,
            });
            continue;
        }
        charged += file.byte_len;
        let mut registration_raced = false;
        let live = (|| {
            let managed = source.managed().ok()?;
            let _lease = managed.as_ref().map(|m| m.try_read()).transpose().ok()?;
            let current = state
                .sources
                .lock()
                .ok()?
                .get_by_id(&source.source_id)
                .ok()?;
            let Some(current) = current else {
                registration_raced = true;
                return None;
            };
            if current.repo_key != source.repo_key || current.root() != source.root() {
                registration_raced = true;
                return None;
            }
            if !current.is_ready() {
                return None;
            }
            let limits = source_capture::CaptureLimits {
                max_files: 1,
                max_file_bytes: file.byte_len,
                max_total_bytes: file.byte_len,
                ..Default::default()
            };
            Some(source_capture::capture_working_tree(
                current.root(),
                &file.source_id,
                std::slice::from_ref(&file.path),
                limits,
            ))
        })();
        let result = if registration_raced {
            R::Raced
        } else {
            match live {
                Some(Ok(capture)) => match capture.file(&file.path) {
                    Ok(current)
                        if current.reference().digest == file.digest
                            && current.reference().byte_len == file.byte_len =>
                    {
                        R::Equal
                    }
                    Ok(_) => R::Changed,
                    Err(_) => R::Unavailable,
                },
                Some(Err(source_capture::CaptureError::Limit(_))) => R::Changed,
                Some(Err(source_capture::CaptureError::Io(ref e)))
                    if e.kind() == std::io::ErrorKind::NotFound =>
                {
                    R::Changed
                }
                _ => R::Unavailable,
            }
        };
        checks.push(O {
            input: I::LiveFileBytes,
            subject,
            result,
        });
    }
    if saved.body.evidence.source_excerpts.is_empty() {
        for input in [
            I::RetainedIntegrity,
            I::LiveFileBytes,
            I::RegistrationRevision,
        ] {
            checks.push(O {
                input,
                subject: "no_pinned_source_ranges".into(),
                result: R::Unavailable,
            });
        }
    }
    if let Ok(before) = before {
        match observed(state, &keys) {
            Ok(after) if after == before => {}
            Ok(_) => checks.push(O {
                input: I::Graph,
                subject: "recovered_graph".into(),
                result: R::Raced,
            }),
            Err(_) => checks.push(O {
                input: I::Graph,
                subject: "recovered_graph".into(),
                result: R::Unavailable,
            }),
        }
    }
    if saved
        .body
        .identity
        .sources
        .iter()
        .any(|s| s.revision.value().is_none())
    {
        limitations.push(
            "Primary-file receipts do not establish a complete current registration revision"
                .into(),
        );
    }
    let freshness = packets::PacketFreshness {
        checked_at: Some(timestamp()?),
        state: packets::assess_freshness(&checks),
        observed_snapshot_id: observed_snapshot,
        checks,
        limitations,
    };
    runtime
        .store
        .lock()
        .map_err(|_| PacketHostError::storage())?
        .record_assessment(id, freshness)
        .map_err(Into::into)
}
#[tauri::command]
pub(crate) async fn assess_work_packet(
    packet_id: String,
    app: tauri::AppHandle,
) -> Result<packets::PacketDetail, PacketHostError> {
    packet_worker(move || {
        let state = app.state::<crate::AppState>();
        assess(&state, &state.work_packets, &packet_id)
    })
    .await
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DraftAdapterPacket {
    pub(crate) language: String,
    pub(crate) content_id: String,
    pub(crate) markdown: String,
}

fn packet_language(body: &packets::PacketBody) -> String {
    body.evidence
        .facts
        .iter()
        .find_map(|fact| fact.fact.props().get("language").and_then(|v| v.as_str()))
        .unwrap_or_default()
        .to_string()
}

pub(crate) fn draft_adapter_packets<R: tauri::Runtime>(
    repo: &str,
    app: &tauri::AppHandle<R>,
    state: &crate::AppState,
) -> Result<Vec<DraftAdapterPacket>, PacketHostError> {
    draft_adapter_packets_with(repo, app, state, &mut || {})
}

/// [`draft_adapter_packets`] with a hook run after the checkout is first
/// checked and before anything is sampled, so tests can change the checkout
/// in between.
fn draft_adapter_packets_with<R: tauri::Runtime>(
    repo: &str,
    app: &tauri::AppHandle<R>,
    state: &crate::AppState,
    before_sampling: &mut dyn FnMut(),
) -> Result<Vec<DraftAdapterPacket>, PacketHostError> {
    adapter_packet_bodies(repo, app, state, before_sampling)?
        .into_iter()
        .map(|body| {
            let language = packet_language(&body);
            let (content_id, markdown) = ingest::adapter_packet::export_adapter_packet(&body)?;
            Ok(DraftAdapterPacket {
                language,
                content_id,
                markdown,
            })
        })
        .collect()
}

/// The adapter packet bodies [`draft_adapter_packets`] exports.
fn adapter_packet_bodies<R: tauri::Runtime>(
    repo: &str,
    app: &tauri::AppHandle<R>,
    state: &crate::AppState,
    before_sampling: &mut dyn FnMut(),
) -> Result<Vec<packets::PacketBody>, PacketHostError> {
    if repo.is_empty() || repo.len() > 256 {
        return Err(PacketHostError::code("unavailable_evidence"));
    }
    let ((nodes, edges), recorded) = {
        let graph = state.graph.lock().map_err(|_| PacketHostError::storage())?;
        graph
            .read_snapshot_with_coverage_bounded(
                snapshot_limits(),
                core_graph::coverage::CoverageReadLimits::default(),
            )
            .map_err(graph_error)?
    };
    let snapshot_id = context_hub::ContextSnapshot::new(nodes, edges)
        .map_err(source_error)?
        .id()
        .to_string();
    let stored = recorded
        .into_iter()
        .find(|stored| stored.record.repo_key == repo)
        .ok_or_else(|| PacketHostError::code("unavailable_evidence"))?;
    let registration_id = stored
        .record
        .registered_source_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| PacketHostError::code("unavailable_evidence"))?;
    let inventory: ingest::coverage::RepoCoverage =
        serde_json::from_str(&stored.record.inventory_json)
            .map_err(|_| PacketHostError::code("unavailable_evidence"))?;
    if inventory.repo != repo || inventory.commit.is_empty() {
        return Err(PacketHostError::code("unavailable_evidence"));
    }
    let source = {
        let sources = state
            .sources
            .lock()
            .map_err(|_| PacketHostError::storage())?;
        sources
            .get_by_repo(repo)
            .map_err(|_| PacketHostError::storage())?
    }
    .filter(|source| source.source_id == registration_id && source.is_ready())
    .ok_or_else(|| PacketHostError::code("unavailable_evidence"))?;
    let operation = crate::SourceOperation::acquire(&state.sources, [(source.clone(), false)])
        .map_err(|_| PacketHostError::code("unavailable_evidence"))?;
    let root = operation
        .root(&source.repo_key)
        .map_err(|_| PacketHostError::code("unavailable_evidence"))?;
    // Samples are read from the checkout as it is now but cited at the
    // recovered commit. Only a clean checkout at that commit pins them: a
    // working-directory recovery has no revision to cite, and a checkout that
    // moved since recovery would cite spans the commit never had.
    let checkout = crate::local_commit::local_commit(root);
    match &checkout {
        crate::local_commit::LocalCommit::Workdir { .. } if inventory.commit == "workdir" => {
            return Err(PacketHostError::code("unpinned_revision"));
        }
        crate::local_commit::LocalCommit::Clean { sha } if *sha == inventory.commit => {}
        _ => return Err(PacketHostError::code("selection_conflict")),
    }
    before_sampling();
    let lookups = crate::preflight_adapter_lookup(app, state, root).map_err(source_error)?;
    let files = ingest::adapter_packet::list_repo_files(root).map_err(source_error)?;
    let context = ingest::adapter_packet::PacketContext {
        snapshot_id: &snapshot_id,
        registration_id: &source.source_id,
        repo_key: &source.repo_key,
        commit_sha: &inventory.commit,
    };
    let bodies = ingest::adapter_packet::adapter_work_packets(&lookups, &files, &context)?;
    // An edit or checkout during the scan would leave samples the commit
    // never had; recheck once they are read, as recovery does before publish.
    if checkout.ensure_unchanged(root).is_err() {
        return Err(PacketHostError::code("selection_conflict"));
    }
    Ok(bodies)
}

/// Saves one drafted adapter packet, named by its content id, so it can be
/// reviewed and then dispatched (ADR-0092). The body is drafted again from
/// the current recovery; a content id it no longer yields fails closed.
pub(crate) fn save_adapter_packet<R: tauri::Runtime>(
    repo: &str,
    content_id: &str,
    app: &tauri::AppHandle<R>,
    state: &crate::AppState,
) -> Result<packets::PacketDetail, PacketHostError> {
    let body = adapter_packet_bodies(repo, app, state, &mut || {})?
        .into_iter()
        .find(|body| body.content_id().ok().as_deref() == Some(content_id))
        .ok_or_else(|| PacketHostError::code("selection_conflict"))?;
    state
        .work_packets
        .store
        .lock()
        .map_err(|_| PacketHostError::storage())?
        .put(body, &[], &timestamp()?)
        .map_err(Into::into)
}

/// Runs the agent for one dispatch: [`acp_host::dispatch`] in the app, a
/// scripted stand-in in tests.
pub(crate) type RunAgent<'a> = dyn FnMut(&acp_host::DispatchRequest<'_>) -> Result<acp_host::Submission, acp_host::DispatchError>
    + 'a;

/// Dispatches a reviewed packet over ACP (ADR-0092): the agent command comes
/// from settings, the checkout from the packet's single source registration
/// at its pinned revision, and the agent works in its own worktree under
/// `worktree_root`. The returned branch is recorded on the packet; nothing
/// is pushed or merged.
pub(crate) fn dispatch_packet(
    state: &crate::AppState,
    packet_id: &str,
    worktree_root: &std::path::Path,
    run: &mut RunAgent<'_>,
) -> Result<packets::PacketDetail, PacketHostError> {
    let detail = state
        .work_packets
        .store
        .lock()
        .map_err(|_| PacketHostError::storage())?
        .get(packet_id)?;
    if detail.review.status != packets::ReviewStatus::Reviewed {
        return Err(PacketHostError::code("not_reviewed"));
    }
    let [source] = detail.body.identity.sources.as_slice() else {
        return Err(PacketHostError::code("unpinned_revision"));
    };
    let base = source
        .revision
        .value()
        .filter(|revision| {
            (7..=64).contains(&revision.len()) && revision.bytes().all(|b| b.is_ascii_hexdigit())
        })
        .ok_or_else(|| PacketHostError::code("unpinned_revision"))?
        .clone();
    let agent = state
        .settings
        .lock()
        .map_err(|_| PacketHostError::storage())?
        .acp_agent()
        .map_err(|_| PacketHostError::code("agent_not_configured"))?
        .ok_or_else(|| PacketHostError::code("agent_not_configured"))?;
    let registered = {
        let sources = state
            .sources
            .lock()
            .map_err(|_| PacketHostError::storage())?;
        sources
            .get_by_repo(&source.repo_key)
            .map_err(|_| PacketHostError::storage())?
    }
    .filter(|registered| registered.source_id == source.registration_id && registered.is_ready())
    .ok_or_else(|| PacketHostError::code("unavailable_evidence"))?;
    let operation = crate::SourceOperation::acquire(&state.sources, [(registered.clone(), false)])
        .map_err(|_| PacketHostError::code("unavailable_evidence"))?;
    let root = operation
        .root(&registered.repo_key)
        .map_err(|_| PacketHostError::code("unavailable_evidence"))?;
    let prompt = ::work_packets::export_markdown(&detail)?;
    std::fs::create_dir_all(worktree_root).map_err(|_| PacketHostError::storage())?;
    let command = acp_host::AgentCommand {
        program: agent.program.into(),
        args: agent.args,
        env: Vec::new(),
    };
    // The host names branches and worktrees from the hexadecimal digest;
    // the store keeps the canonical `work-packet-v1:<hex>` id.
    let digest = packet_id
        .rsplit_once(':')
        .map_or(packet_id, |(_, digest)| digest);
    let submission = run(&acp_host::DispatchRequest {
        packet_id: digest,
        prompt: &prompt,
        repo: root,
        base_rev: &base,
        worktree_root,
        agent: &command,
        limits: acp_host::Limits::default(),
    })
    .map_err(|error| match error {
        acp_host::DispatchError::Worktree(acp_host::WorktreeError::Exists(_)) => {
            PacketHostError::code("already_dispatched")
        }
        acp_host::DispatchError::Spawn(_) => PacketHostError::code("agent_unavailable"),
        _ => PacketHostError::code("dispatch_failed"),
    })?;
    drop(operation);
    let mut changed_paths = submission.changed_paths;
    let omitted = changed_paths
        .len()
        .saturating_sub(packets::MAX_DISPATCH_PATHS);
    changed_paths.truncate(packets::MAX_DISPATCH_PATHS);
    let record = packets::DispatchRecord {
        reviewed_revision: detail.review.revision,
        branch: submission.branch,
        base_revision: submission.base_rev,
        head_revision: submission.head_rev,
        stop: stop_label(&submission.stop).into(),
        changed_paths,
        changed_paths_omitted: u32::try_from(omitted).unwrap_or(u32::MAX),
        refused_requests: u32::try_from(submission.refused.len()).unwrap_or(u32::MAX),
    };
    state
        .work_packets
        .store
        .lock()
        .map_err(|_| PacketHostError::storage())?
        .record_dispatch(packet_id, &record, &timestamp()?)
        .map_err(Into::into)
}

fn stop_label(stop: &acp_host::StopReason) -> &'static str {
    use acp_host::StopReason::*;
    match stop {
        EndTurn => "end_turn",
        MaxTokens => "max_tokens",
        MaxTurnRequests => "max_turn_requests",
        Refusal => "refusal",
        Cancelled => "cancelled",
        TimedOut => "timed_out",
        AgentExited => "agent_exited",
        ProtocolError(_) => "protocol_error",
    }
}

#[tauri::command]
pub(crate) async fn draft_adapter_work_packets(
    repo: String,
    app: tauri::AppHandle,
) -> Result<Vec<DraftAdapterPacket>, PacketHostError> {
    packet_worker(move || {
        let state = app.state::<crate::AppState>();
        draft_adapter_packets(&repo, &app, &state)
    })
    .await
}

/// Saves a drafted adapter packet for review (ADR-0092).
#[tauri::command]
pub(crate) async fn save_adapter_work_packet(
    repo: String,
    content_id: String,
    app: tauri::AppHandle,
) -> Result<packets::PacketDetail, PacketHostError> {
    packet_worker(move || {
        let state = app.state::<crate::AppState>();
        save_adapter_packet(&repo, &content_id, &app, &state)
    })
    .await
}

/// Dispatches a reviewed packet to the configured ACP agent in its own
/// worktree under the app data directory, and records the branch it
/// returns (ADR-0092). Runs until the agent's turn ends or its limit passes.
#[tauri::command]
pub(crate) async fn dispatch_work_packet(
    packet_id: String,
    app: tauri::AppHandle,
) -> Result<packets::PacketDetail, PacketHostError> {
    packet_worker(move || {
        let state = app.state::<crate::AppState>();
        let worktree_root = app
            .path()
            .app_data_dir()
            .map_err(|_| PacketHostError::storage())?
            .join("acp-worktrees");
        dispatch_packet(&state, &packet_id, &worktree_root, &mut acp_host::dispatch)
    })
    .await
}

#[cfg(all(test, not(app_test_group = "acceptance")))]
mod acceptance_tests;

#[cfg(all(test, not(app_test_group = "acceptance")))]
mod tests {
    use super::*;
    use core_graph::{GraphStore, Node};

    #[test]
    fn packet_tray_uses_host_digests_and_rejects_missing_facts() {
        let mut graph = SqliteGraphStore::open_in_memory().unwrap();
        let node = Node {
            id: "actual-node".into(),
            label: "Symbol".into(),
            props: serde_json::json!({"name":"actual source"}),
        };
        graph.put_node(&node).unwrap();
        let decisions = agents::DecisionLog::open(":memory:").unwrap();
        let key = FactKey::from_node(&node);
        let selected = prepare_selection(&graph, &decisions, &[key.clone(), key], &[]).unwrap();
        assert_eq!(selected.facts.len(), 1);
        assert_eq!(
            selected.facts[0].expected_digest,
            core_graph::source::node_digest(&node).unwrap()
        );
        assert!(selected.snapshot_id.starts_with("context-v1:"));
        assert_eq!(
            prepare_selection(
                &graph,
                &decisions,
                &[FactKey::Node {
                    id: "absent".into()
                }],
                &[]
            )
            .unwrap_err(),
            "selection_conflict"
        );
    }

    fn write_file(root: &std::path::Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn draft_adapter_work_packets_for_recovered_repos() {
        // AC-0371 (#1101): draft_adapter_work_packets returns the #1084 export
        // for each uncovered language of a recovered repo, is byte-identical
        // across calls, returns empty when all looked-up cells have a
        // declaring source, fails closed for unknown or unrecovered repos, and
        // mutates nothing in the graph, packet store, repo, or register.
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("app");
        let ruby_dir = directory.path().join("ruby");
        let terraform_dir = directory.path().join("terraform");
        let unrecovered_dir = directory.path().join("unrecovered");
        std::fs::create_dir_all(&data).unwrap();

        // Clean git checkouts, so samples are pinned to the recorded commit.
        // `.cartograph/` is ignored so installing a plugin keeps it clean.
        crate::local_commit::tests::init_clean_repo(
            &ruby_dir,
            &[
                (".gitignore", b"vendor/\n.cartograph/\n"),
                ("README.md", b"# fixture\n"),
                ("Gemfile", b"source 'https://rubygems.org'\n"),
                (
                    "app/models/user.rb",
                    b"require 'json'\n\nclass User\n  def name\n    'u'\n  end\nend\n",
                ),
                (
                    "app/models/order.rb",
                    b"class Order\n  def total\n    1\n  end\nend\n",
                ),
                (
                    "app/controllers/users_controller.rb",
                    b"class UsersController\n  def index\n    User.new.name\n  end\nend\n",
                ),
                ("lib/util.rb", b"module Util\n  def self.id(x) = x\nend\n"),
                ("empty.rb", b""),
            ],
        );
        write_file(&ruby_dir, "vendor/ignored.rb", "puts 'vendored'\n");
        crate::local_commit::tests::init_clean_repo(
            &terraform_dir,
            &[(
                "main.tf",
                b"resource \"aws_s3_bucket\" \"logs\" {\n  bucket = \"logs\"\n}\n",
            )],
        );
        write_file(&unrecovered_dir, "lib/only.rb", "puts 'hi'\n");

        let state = crate::registered_source_tests::app_state(&data);
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .expect("mock app"),
        );
        app.manage(crate::PreflightRuns::default());
        let handle = app.handle();

        // Unknown repo and unrecovered registered repo both fail closed.
        assert_eq!(
            draft_adapter_packets("unknown/repo", handle, &state)
                .unwrap_err()
                .code,
            "unavailable_evidence"
        );
        let unrecovered = crate::register_local_source(&state, &unrecovered_dir).unwrap();
        assert_eq!(
            draft_adapter_packets(&unrecovered.repo_key, handle, &state)
                .unwrap_err()
                .code,
            "unavailable_evidence"
        );

        // Recover the Ruby fixture repo and draft adapter work packets.
        let ruby_source = crate::register_local_source(&state, &ruby_dir).unwrap();
        let operation =
            crate::source_operation(&state, vec![(ruby_source.clone(), false)]).unwrap();
        let (_, execution) = crate::start_job(&state, &ruby_source.ingest_job_kind()).unwrap();
        let summary =
            crate::run_ingest(&ruby_source, &operation, &execution, handle, &state).unwrap();
        drop(operation);

        let snapshot_before = prepare_selection(
            &state.graph.lock().unwrap(),
            &state.decisions.lock().unwrap(),
            &[],
            &[],
        )
        .unwrap()
        .snapshot_id;
        let packets_before = state
            .work_packets
            .store
            .lock()
            .unwrap()
            .list_after(20, None)
            .unwrap()
            .len();
        let findings_before =
            serde_json::to_value(state.findings.lock().unwrap().list().unwrap()).unwrap();
        let files_before = ingest::adapter_packet::list_repo_files(ruby_source.root()).unwrap();
        let file_bytes_before: Vec<_> = files_before
            .iter()
            .map(|file| std::fs::read(&file.path).unwrap())
            .collect();

        let first = draft_adapter_packets(&ruby_source.repo_key, handle, &state).unwrap();
        let second = draft_adapter_packets(&ruby_source.repo_key, handle, &state).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first, second);
        assert_eq!(first[0].language, "Ruby");

        let lookups = crate::preflight_adapter_lookup(handle, &state, ruby_source.root()).unwrap();
        let expected_bodies = ingest::adapter_packet::adapter_work_packets(
            &lookups,
            &files_before,
            &ingest::adapter_packet::PacketContext {
                snapshot_id: &snapshot_before,
                registration_id: &ruby_source.source_id,
                repo_key: &ruby_source.repo_key,
                commit_sha: &summary.coverage[0].commit,
            },
        )
        .unwrap();
        assert_eq!(expected_bodies.len(), 1);
        let (expected_id, expected_markdown) =
            ingest::adapter_packet::export_adapter_packet(&expected_bodies[0]).unwrap();
        assert_eq!(first[0].content_id, expected_id);
        assert_eq!(first[0].markdown, expected_markdown);

        let snapshot_after = prepare_selection(
            &state.graph.lock().unwrap(),
            &state.decisions.lock().unwrap(),
            &[],
            &[],
        )
        .unwrap()
        .snapshot_id;
        let packets_after = state
            .work_packets
            .store
            .lock()
            .unwrap()
            .list_after(20, None)
            .unwrap()
            .len();
        let findings_after =
            serde_json::to_value(state.findings.lock().unwrap().list().unwrap()).unwrap();
        let files_after = ingest::adapter_packet::list_repo_files(ruby_source.root()).unwrap();
        let file_bytes_after: Vec<_> = files_after
            .iter()
            .map(|file| std::fs::read(&file.path).unwrap())
            .collect();
        assert_eq!(snapshot_after, snapshot_before);
        assert_eq!(packets_after, 0);
        assert_eq!(packets_after, packets_before);
        assert_eq!(findings_after, findings_before);
        assert_eq!(files_after, files_before);
        assert_eq!(file_bytes_after, file_bytes_before);

        // An ungated plugin declaring Ruby's core cells counts as declared, so
        // no packet is drafted once those cells are declared.
        let adapters_dir = ruby_source.root().join(".cartograph/adapters");
        std::fs::create_dir_all(&adapters_dir).unwrap();
        std::fs::write(adapters_dir.join("t0.plugin-ruby.wasm"), b"\0asm\x01\0\0\0").unwrap();
        std::fs::write(
            adapters_dir.join("t0.plugin-ruby.golden.json"),
            serde_json::json!({
                "extensions": ["rb"],
                "cases": [{"path": "a.rb", "source": "x = 1\n", "nodes": [], "edges": []}],
                "coverage": [
                    {"language": "Ruby", "framework": "core", "capability": "imports", "support": "supported", "cases": ["a.rb"]},
                    {"language": "Ruby", "framework": "core", "capability": "calls", "support": "supported", "cases": ["a.rb"]},
                    {"language": "Ruby", "framework": "core", "capability": "types", "support": "supported", "cases": ["a.rb"]}
                ]
            })
            .to_string(),
        )
        .unwrap();
        assert!(
            draft_adapter_packets(&ruby_source.repo_key, handle, &state)
                .unwrap()
                .is_empty()
        );

        // A recovered repo whose compiled-in cells are all supported (Terraform;
        // every code language now has a partial cell) also returns an empty list.
        let terraform_source = crate::register_local_source(&state, &terraform_dir).unwrap();
        let operation =
            crate::source_operation(&state, vec![(terraform_source.clone(), false)]).unwrap();
        let (_, execution) = crate::start_job(&state, &terraform_source.ingest_job_kind()).unwrap();
        crate::run_ingest(&terraform_source, &operation, &execution, handle, &state).unwrap();
        drop(operation);
        assert!(
            draft_adapter_packets(&terraform_source.repo_key, handle, &state)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_checkout_that_moved_since_recovery_drafts_no_packet() {
        // AC-0371 (#1101): samples would be read from a checkout the recovered
        // commit no longer describes, so the draft fails closed.
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("app");
        std::fs::create_dir_all(&data).unwrap();
        let project = directory.path().join("ruby");
        crate::local_commit::tests::init_clean_repo(
            &project,
            &[(
                "lib/user.rb",
                b"class User\n  def name\n    'u'\n  end\nend\n",
            )],
        );
        let state = crate::registered_source_tests::app_state(&data);
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .expect("mock app"),
        );
        app.manage(crate::PreflightRuns::default());
        let handle = app.handle();
        let source = crate::register_local_source(&state, &project).unwrap();
        let operation = crate::source_operation(&state, vec![(source.clone(), false)]).unwrap();
        let (_, execution) = crate::start_job(&state, &source.ingest_job_kind()).unwrap();
        crate::run_ingest(&source, &operation, &execution, handle, &state).unwrap();
        drop(operation);
        assert_eq!(
            draft_adapter_packets(&source.repo_key, handle, &state)
                .unwrap()
                .len(),
            1
        );

        std::fs::write(project.join("lib/user.rb"), "class User\nend\n").unwrap();
        assert_eq!(
            draft_adapter_packets(&source.repo_key, handle, &state)
                .unwrap_err()
                .code,
            "selection_conflict"
        );
    }

    /// Register and recover `root` as a local source.
    fn recovered(
        state: &crate::AppState,
        handle: &tauri::AppHandle<tauri::test::MockRuntime>,
        root: &std::path::Path,
    ) -> crate::sources::RegisteredSource {
        let source = crate::register_local_source(state, root).unwrap();
        let operation = crate::source_operation(state, vec![(source.clone(), false)]).unwrap();
        let (_, execution) = crate::start_job(state, &source.ingest_job_kind()).unwrap();
        crate::run_ingest(&source, &operation, &execution, handle, state).unwrap();
        source
    }

    #[test]
    fn a_working_directory_recovery_has_no_revision_to_pin_samples_to() {
        // AC-0371 (#1101): a source recovered as `workdir` has no commit the
        // samples could be cited at, so no packet is drafted from it.
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("app");
        std::fs::create_dir_all(&data).unwrap();
        let project = directory.path().join("ruby");
        write_file(&project, "lib/user.rb", "class User\nend\n");
        let state = crate::registered_source_tests::app_state(&data);
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .expect("mock app"),
        );
        app.manage(crate::PreflightRuns::default());
        let source = recovered(&state, app.handle(), &project);
        assert_eq!(
            draft_adapter_packets(&source.repo_key, app.handle(), &state)
                .unwrap_err()
                .code,
            "unpinned_revision"
        );
    }

    #[test]
    fn a_checkout_edited_while_sampling_drafts_no_packet() {
        // AC-0371 (#1101): the checkout is rechecked once samples are read,
        // so an edit during the scan fails closed instead of citing bytes the
        // recovered commit never had.
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("app");
        std::fs::create_dir_all(&data).unwrap();
        let project = directory.path().join("ruby");
        crate::local_commit::tests::init_clean_repo(
            &project,
            &[(
                "lib/user.rb",
                b"class User\n  def name\n    'u'\n  end\nend\n",
            )],
        );
        let state = crate::registered_source_tests::app_state(&data);
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .expect("mock app"),
        );
        app.manage(crate::PreflightRuns::default());
        let source = recovered(&state, app.handle(), &project);
        let file = project.join("lib/user.rb");
        let result =
            draft_adapter_packets_with(&source.repo_key, app.handle(), &state, &mut || {
                std::fs::write(&file, "class User\nend\n").unwrap();
            });
        assert_eq!(result.unwrap_err().code, "selection_conflict");
    }

    #[test]
    fn a_saved_adapter_packet_dispatches_only_when_reviewed_and_records_its_branch() {
        // AC-1348 (ADR-0092, #998).
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("app");
        std::fs::create_dir_all(&data).unwrap();
        let project = directory.path().join("ruby");
        let commit = crate::local_commit::tests::init_clean_repo(
            &project,
            &[(
                "lib/user.rb",
                b"class User\n  def name\n    'u'\n  end\nend\n",
            )],
        );
        let state = crate::registered_source_tests::app_state(&data);
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .expect("mock app"),
        );
        app.manage(crate::PreflightRuns::default());
        let handle = app.handle();
        let source = recovered(&state, handle, &project);
        let drafted = draft_adapter_packets(&source.repo_key, handle, &state).unwrap();
        assert_eq!(
            save_adapter_packet(&source.repo_key, "not-a-drafted-id", handle, &state)
                .unwrap_err()
                .code,
            "selection_conflict"
        );
        let saved =
            save_adapter_packet(&source.repo_key, &drafted[0].content_id, handle, &state).unwrap();
        assert_eq!(saved.packet_id, drafted[0].content_id);
        assert_eq!(saved.execution, packets::ExecutionState::Unoffered);
        let id = saved.packet_id;
        let worktrees = directory.path().join("worktrees");
        let mut never = |_: &acp_host::DispatchRequest<'_>| -> Result<
            acp_host::Submission,
            acp_host::DispatchError,
        > { panic!("the agent ran") };

        assert_eq!(
            dispatch_packet(&state, &id, &worktrees, &mut never)
                .unwrap_err()
                .code,
            "not_reviewed"
        );
        state
            .work_packets
            .store
            .lock()
            .unwrap()
            .review(&id, 0, packets::ReviewDecision::Reviewed, None, "reviewed")
            .unwrap();
        assert_eq!(
            dispatch_packet(&state, &id, &worktrees, &mut never)
                .unwrap_err()
                .code,
            "agent_not_configured"
        );
        // Settings require an absolute program path, which differs by OS.
        let program = if cfg!(windows) {
            r"C:\agent\bin\acp.exe"
        } else {
            "/opt/agent/bin/acp"
        };
        let agent = crate::settings::AcpAgentCommand {
            program: program.into(),
            args: vec!["--acp".into()],
        };
        state
            .settings
            .lock()
            .unwrap()
            .set_acp_agent(Some(&agent))
            .unwrap();

        let head = "c".repeat(40);
        let mut ran = 0;
        let mut agent_run = |request: &acp_host::DispatchRequest<'_>| {
            ran += 1;
            // The host refuses any id that is not hexadecimal (AC-1344).
            assert_eq!(Some(request.packet_id), id.strip_prefix("work-packet-v1:"));
            assert!(
                request.packet_id.len() == 64
                    && request.packet_id.bytes().all(|b| b.is_ascii_hexdigit())
            );
            assert_eq!(request.base_rev, commit);
            assert_eq!(request.repo, source.root());
            assert_eq!(request.worktree_root, worktrees);
            assert_eq!(request.agent.program, std::path::Path::new(program));
            assert_eq!(request.agent.args, ["--acp"]);
            assert!(
                request.prompt.contains(&id),
                "the prompt is the packet export"
            );
            Ok(acp_host::Submission {
                packet_id: id.clone(),
                branch: "cartograph/packet-0123456789ab".into(),
                worktree: worktrees.join("packet-0123456789ab"),
                base_rev: commit.clone(),
                head_rev: Some(head.clone()),
                changed_paths: (0..205).map(|n| format!("src/{n}.rb")).collect(),
                stop: acp_host::StopReason::EndTurn,
                transcript: Vec::new(),
                transcript_truncated: false,
                refused: vec![acp_host::Refusal {
                    method: "terminal/create".into(),
                    detail: "not offered".into(),
                }],
            })
        };
        let dispatched = dispatch_packet(&state, &id, &worktrees, &mut agent_run).unwrap();
        assert_eq!(ran, 1);
        assert_eq!(dispatched.execution, packets::ExecutionState::Submitted);
        let record = dispatched.dispatch.unwrap();
        assert_eq!(record.reviewed_revision, 1);
        assert_eq!(record.head_revision.as_deref(), Some(head.as_str()));
        assert_eq!(record.stop, "end_turn");
        assert_eq!(record.changed_paths.len(), packets::MAX_DISPATCH_PATHS);
        assert_eq!(record.changed_paths_omitted, 5);
        assert_eq!(record.refused_requests, 1);

        let mut exists = |_: &acp_host::DispatchRequest<'_>| {
            Err(acp_host::DispatchError::Worktree(
                acp_host::WorktreeError::Exists(worktrees.join("packet-0123456789ab")),
            ))
        };
        assert_eq!(
            dispatch_packet(&state, &id, &worktrees, &mut exists)
                .unwrap_err()
                .code,
            "already_dispatched"
        );
        let reopened = state.work_packets.store.lock().unwrap().get(&id).unwrap();
        assert_eq!(reopened.execution, packets::ExecutionState::Submitted);
    }
}
