//! Capture-only, durable preparation. No provider object or execution lease is accepted.
use crate::AppState;
use agents::TaskRangeRole;
use agents::investigation::*;
use core_graph::{
    GraphStore, SnapshotReadLimits,
    source::{FactKey, FactSourceState},
};
use core_prov::content_hash;
use ingest::coverage::{MissingInput, ProducerCoverage, ProducerKind, ProducerState, RepoCoverage};
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use tauri::Manager;

pub(super) const ANALYST_QUESTION: &str = "For this synthetic control, describe the recorded analysis limits and what the available source establishes. Inspect the saved inventory and retained source before answering, and distinguish coverage limits from source-supported behavior.";
pub(super) const AUDITOR_QUESTION: &str = "For this synthetic control, audit the saved parent's findings against the recorded limits and retained source. Perform your own inventory and source inspection, keep parent findings attributed as history, and explain only claims supported by the evidence supplied to this invocation.";
const MAX_PREPARATION_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in super::super) struct PreparationBinding {
    pub run_id: String,
    pub protocol_hash: String,
    pub source_commit: String,
    pub source_tree: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in super::super) struct CaptureManifest {
    pub schema_version: u32,
    pub binding: PreparationBinding,
    pub provider: InvestigationProvider,
    pub source_hash: String,
    pub inventory_hash: String,
    pub graph_before_hash: String,
    pub context_id: String,
    pub config_hash: String,
    pub registered_source_id: String,
    pub analyst_id: String,
    pub conversation_id: String,
    pub analyst_request_hash: String,
    pub auditor_request_hash: String,
    pub analyst_wire_hash: String,
    pub auditor_wire_hash: String,
}
pub(in super::super) struct PreparedState {
    pub app: tauri::App<tauri::test::MockRuntime>,
    pub app_data: PathBuf,
    pub analyst_request: StagedObligationRequest,
    pub auditor_request: StagedObligationRequest,
    pub analyst_id: String,
    pub auditor_id: Option<String>,
    pub conversation_id: String,
    pub graph_before_hash: String,
    pub manifest: CaptureManifest,
    pub manifest_hash: String,
    _guards: Vec<crate::primary_source::RetentionGuard>,
}
// Closed family selection: V7 keeps its original manifest and request bytes.
#[derive(Clone, Copy)]
pub(in super::super) enum Family {
    V7,
    Expanded,
}
impl Family {
    fn policy(self) -> &'static InvestigationPolicy {
        match self {
            Self::V7 => &INVESTIGATION_V1_POLICY,
            Self::Expanded => &INVESTIGATION_V2_POLICY,
        }
    }
    fn manifest_schema(self) -> u32 {
        match self {
            Self::V7 => 1,
            Self::Expanded => 2,
        }
    }
    fn envelope(self) -> (u32, &'static str) {
        match self {
            Self::V7 => (1, "investigation-completion-obligations@1"),
            Self::Expanded => (2, "investigation-completion-obligations@2"),
        }
    }
    fn roles(self) -> [&'static str; 2] {
        match self {
            Self::V7 => ["domain-analyst@7", "evidence-auditor@7"],
            Self::Expanded => ["domain-analyst@10", "evidence-auditor@10"],
        }
    }
}
// Independently selected synthetic fixture; never inferred from a saved manifest.
#[derive(Clone, Copy)]
pub(in super::super) enum CaptureFixture {
    Baseline,
    Capacity,
}
impl CaptureFixture {
    fn validate(self, family: Family) -> Result<(), String> {
        if matches!((family, self), (Family::V7, Self::Capacity)) {
            return Err("Capacity fixture requires the expanded family.".into());
        }
        Ok(())
    }
    fn source(self) -> String {
        match self {
            Self::Baseline => super::super::super::ORIGINAL.into(),
            Self::Capacity => format!(
                "export function enabled(ok: boolean) {{ const limited = ok /*{}*/ === false; if (limited) return false; }}\n",
                "\u{1}".repeat(7 * 1024)
            ),
        }
    }
}
fn json(value: &impl Serialize) -> Result<Vec<u8>, String> {
    crate::investigations::context::bounded_json(value, MAX_PREPARATION_BYTES)
        .map_err(|error| error.to_string())
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("Frozen artifact must be a regular file.".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_PREPARATION_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_PREPARATION_BYTES {
        return Err("Frozen artifact exceeds its bound.".into());
    }
    Ok(bytes)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_PREPARATION_BYTES {
        return Err("Frozen artifact exceeds its bound.".into());
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| error.to_string())
}
fn hash(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
pub(in super::super) fn validate_binding(binding: &PreparationBinding) -> Result<(), String> {
    if binding.run_id.is_empty()
        || binding.run_id.len() > 120
        || !binding
            .run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_:".contains(&byte))
        || !hash(&binding.protocol_hash, 64)
        || !hash(&binding.source_commit, 40)
        || !hash(&binding.source_tree, 40)
    {
        return Err("Invalid preparation binding.".into());
    }
    Ok(())
}
fn inventory(repo: &str) -> Result<String, String> {
    serde_json::to_string(&RepoCoverage {
        inventory: ingest::coverage::INVENTORY_ID.into(), repo: repo.into(), commit: "workdir".into(), commit_note: None, layer_hints: vec![],
        producers: vec![ProducerCoverage { id: "harness-generated-synthetic-control@1".into(), kind: ProducerKind::Observational, covers: "synthetic control input".into(), state: ProducerState::MissingInput, files: 0, nodes: 0, edges: 0, inputs: vec![], stopping_reason: "Harness-generated synthetic control inventory: the named control producer did not run because its optional control input was not declared. This record is not a full ingest-pipeline coverage claim.".into(), refusals: None }],
        missing_inputs: vec![MissingInput { input: "synthetic_control_input".into(), producer: "harness-generated-synthetic-control@1".into(), reason: "Optional synthetic control input was not declared for this recorded scope.".into() }], unsupported: vec![], limited: true, documents: None,
    }).map_err(|error| error.to_string())
}
fn app(state: AppState) -> Result<tauri::App<tauri::test::MockRuntime>, String> {
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .map_err(|error| error.to_string())?;
    app.manage(state);
    Ok(app)
}
fn publish_inventory(
    app: &tauri::App<tauri::test::MockRuntime>,
    family: Family,
) -> Result<(String, String, String), String> {
    let state = app.state::<AppState>();
    let scope = crate::context::coverage_scope(app.handle(), &state)?;
    if scope.len() != 1 {
        return Err("Synthetic control requires one registered repository.".into());
    }
    let repo = &scope[0];
    let registered = repo
        .registered_source_id
        .clone()
        .ok_or("Synthetic registration is missing.")?;
    let inventory = inventory(&repo.repo_key)?;
    let mut graph = state.graph.lock().map_err(|_| "Graph store unavailable.")?;
    let snapshot = graph
        .read_snapshot_bounded(SnapshotReadLimits::default())
        .map_err(|error| error.to_string())?;
    let keys = snapshot
        .0
        .iter()
        .map(|node| FactKey::Node {
            id: node.id.clone(),
        })
        .chain(snapshot.1.iter().map(|edge| FactKey::Edge {
            source: edge.src.clone(),
            label: edge.label.clone(),
            destination: edge.dst.clone(),
        }))
        .collect::<Vec<_>>();
    let selection = graph
        .read_source_selection_snapshot_bounded(&keys, SnapshotReadLimits::default())
        .map_err(|error| error.to_string())?;
    if selection.graph != snapshot {
        return Err("Graph changed before synthetic inventory publication.".into());
    }
    let bindings = selection
        .selections
        .into_iter()
        .filter_map(|selection| match selection.state {
            FactSourceState::Present { binding, .. } => Some(binding),
            _ => None,
        })
        .collect::<Vec<_>>();
    let record = core_graph::coverage::CoverageRecord {
        repo_key: repo.repo_key.clone(),
        registered_source_id: Some(registered.clone()),
        ingest_id: match family {
            Family::V7 => "v7-synthetic-control-preparation@1",
            Family::Expanded => "expanded-synthetic-control-preparation@1",
        }
        .into(),
        config_hash: repo.config_hash.clone(),
        inventory_json: inventory.clone(),
    };
    if !graph
        .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
            &snapshot,
            &core_graph::GraphPatch::default(),
            &repo.repo_key,
            &bindings,
            Some(&record),
            core_graph::source::SourceOmissions::NONE,
        )
        .map_err(|error| error.to_string())?
        || graph.read_snapshot().map_err(|error| error.to_string())? != snapshot
    {
        return Err("Synthetic inventory changed the captured graph.".into());
    }
    Ok((inventory, repo.config_hash.clone(), registered))
}
fn resolve_source(
    state: &AppState,
    fixture: CaptureFixture,
) -> Result<(Obligation, crate::primary_source::RetentionGuard), String> {
    let nodes = state
        .graph
        .lock()
        .map_err(|_| "Graph store unavailable.")?
        .nodes_with_label("BusinessRule")
        .map_err(|error| error.to_string())?;
    if nodes.len() != 1 {
        return Err("Control requires exactly one BusinessRule.".into());
    }
    let fact = agents::TaskFactKey::Node {
        id: nodes[0].id.clone(),
    };
    let description =
        crate::primary_source::describe(state, &crate::task_evidence::graph_fact(&fact))?;
    let mut targets = Vec::new();
    for range in description.ranges {
        let (target, guard) = crate::investigations::inspection::resolve(
            state,
            &fact,
            &description.graph_snapshot_id,
            &description.receipt_id,
            range.index,
        )?;
        if target.role == TaskRangeRole::DefinitionInitializer {
            targets.push((target, guard));
        }
    }
    if targets.len() != 1 {
        return Err("Initializer target must be unique.".into());
    }
    let (target, guard) = targets.pop().ok_or("Initializer target is missing.")?;
    let original = fixture.source();
    if target.captured.file.digest != content_hash(original.as_bytes())
        || target.captured.file.byte_len != original.len() as u64
        || target.captured.file.path != "rule.ts"
    {
        return Err("Retained source differs from the frozen control.".into());
    }
    let mut obligation = Obligation::Source {
        obligation_id: String::new(),
        target: Box::new(target),
    };
    let identity = obligation.content_id().map_err(|error| error.to_string())?;
    if let Obligation::Source { obligation_id, .. } = &mut obligation {
        *obligation_id = identity;
    }
    Ok((obligation, guard))
}
fn resolve_request(
    app: &tauri::App<tauri::test::MockRuntime>,
    specialist: &str,
    nonce: &str,
    question: &str,
    family: Family,
    fixture: CaptureFixture,
) -> Result<
    (
        StagedObligationRequest,
        crate::primary_source::RetentionGuard,
    ),
    String,
> {
    let state = app.state::<AppState>();
    // Each role calls both production resolvers independently; never clone the
    // analyst selection as an assumed auditor authority.
    let (source, guard) = resolve_source(&state, fixture)?;
    let inventory = crate::investigations::completion_choices::control_inventory(
        app.handle(),
        &state,
        &InvestigationScope::All,
    )?;
    let graph = match &source {
        Obligation::Source { target, .. } => target.graph_snapshot_id.clone(),
        _ => return Err("Source resolver returned the wrong kind.".into()),
    };
    let context = match &inventory {
        Obligation::Inventory { target, .. } => target.context_id.clone(),
        _ => return Err("Inventory resolver returned the wrong kind.".into()),
    };
    let request = StagedObligationRequest {
        schema_version: match family {
            Family::V7 => 4,
            Family::Expanded => 7,
        },
        request_nonce: nonce.into(),
        specialist_id: specialist.into(),
        obligations: ObligationEnvelope {
            schema_version: family.envelope().0,
            contract: family.envelope().1.into(),
            obligations: vec![source, inventory],
        },
        inspection_requirements: None,
        question: question.into(),
        scope: InvestigationScope::All,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: family.policy().profile().into(),
        expected_graph_revision: Some(graph),
        expected_context_id: context,
        conversation_id: None,
        parent_id: None,
    }
    .normalized_with_policy(family.policy())
    .map_err(|error| error.to_string())?;
    crate::investigations::completion_choices::validate_request(app.handle(), &state, &request)?;
    Ok((request, guard))
}

pub(super) fn prepare(
    run_dir: &Path,
    provider: &InvestigationProvider,
    binding: PreparationBinding,
) -> Result<PreparedState, String> {
    prepare_family(run_dir, provider, binding, Family::V7)
}

pub(in super::super) fn prepare_family(
    run_dir: &Path,
    provider: &InvestigationProvider,
    binding: PreparationBinding,
    family: Family,
) -> Result<PreparedState, String> {
    prepare_fixture(run_dir, provider, binding, family, CaptureFixture::Baseline)
}
pub(in super::super) fn prepare_fixture(
    run_dir: &Path,
    provider: &InvestigationProvider,
    binding: PreparationBinding,
    family: Family,
    fixture: CaptureFixture,
) -> Result<PreparedState, String> {
    fixture.validate(family)?;
    validate_binding(&binding)?;
    if provider.mode != InvestigationProviderMode::Local || !provider.available {
        return Err("Control requires its declared available local descriptor.".into());
    }
    let run_dir = run_dir.canonicalize().map_err(|error| error.to_string())?;
    let fixture_dir = run_dir.join("fixture");
    std::fs::create_dir(&fixture_dir).map_err(|error| error.to_string())?;
    let (state, app_data, graph_before) =
        super::super::super::captured_fixture_state(&fixture_dir, &fixture.source(), false, None);
    let graph_before_hash = content_hash(graph_before.as_bytes());
    let app = app(state)?;
    let (inventory, config_hash, registered_source_id) = publish_inventory(&app, family)?;
    let (analyst_request, analyst_guard) = resolve_request(
        &app,
        family.roles()[0],
        &format!("{}-analyst", binding.run_id),
        ANALYST_QUESTION,
        family,
        fixture,
    )?;
    let (mut auditor_request, auditor_guard) = resolve_request(
        &app,
        family.roles()[1],
        &format!("{}-auditor", binding.run_id),
        AUDITOR_QUESTION,
        family,
        fixture,
    )?;
    let state = app.state::<AppState>();
    let mut jobs = state.jobs.lock().map_err(|_| "Job store unavailable.")?;
    let started = match family {
        Family::V7 => jobs.start_completion_investigation(&analyst_request, Some(provider)),
        Family::Expanded => jobs.start_next_prompt_investigation(&analyst_request, Some(provider)),
    }
    .map_err(|error| error.to_string())?;
    drop(jobs);
    if !started.created || started.detail.summary.status != InvestigationStatus::Queued {
        return Err("Preparation must create one new queued analyst.".into());
    }
    let analyst_id = started.detail.summary.investigation_id;
    let conversation_id = started.detail.summary.conversation_id;
    auditor_request.conversation_id = Some(conversation_id.clone());
    auditor_request.parent_id = Some(analyst_id.clone());
    auditor_request
        .normalized_with_policy(family.policy())
        .map_err(|error| error.to_string())?;
    let analyst_wire = json(&analyst_request)?;
    let auditor_wire = json(&auditor_request)?;
    let manifest = CaptureManifest {
        schema_version: family.manifest_schema(),
        binding,
        provider: provider.clone(),
        source_hash: content_hash(fixture.source().as_bytes()),
        inventory_hash: content_hash(inventory.as_bytes()),
        graph_before_hash: graph_before_hash.clone(),
        context_id: analyst_request.expected_context_id.clone(),
        config_hash,
        registered_source_id,
        analyst_id: analyst_id.clone(),
        conversation_id: conversation_id.clone(),
        analyst_request_hash: analyst_request
            .intent_hash_with_policy(family.policy())
            .map_err(|error| error.to_string())?,
        auditor_request_hash: auditor_request
            .intent_hash_with_policy(family.policy())
            .map_err(|error| error.to_string())?,
        analyst_wire_hash: content_hash(&analyst_wire),
        auditor_wire_hash: content_hash(&auditor_wire),
    };
    let bytes = json(&manifest)?;
    let manifest_hash = content_hash(&bytes);
    for (name, bytes) in [
        ("source-control.ts", fixture.source().into_bytes()),
        ("inventory.json", inventory.into_bytes()),
        ("analyst-request.json", analyst_wire),
        ("auditor-request.json", auditor_wire),
        ("capture-manifest.json", bytes),
        ("capture-manifest.hash", manifest_hash.as_bytes().to_vec()),
    ] {
        write_new(&run_dir.join(name), &bytes)?;
    }
    Ok(PreparedState {
        app,
        app_data,
        analyst_request,
        auditor_request,
        analyst_id,
        auditor_id: None,
        conversation_id,
        graph_before_hash,
        manifest,
        manifest_hash,
        _guards: vec![analyst_guard, auditor_guard],
    })
}

pub(super) fn reopen(run_dir: &Path) -> Result<PreparedState, String> {
    reopen_family(run_dir, Family::V7, None)
}

pub(in super::super) fn reopen_family(
    run_dir: &Path,
    family: Family,
    expected_binding: Option<&PreparationBinding>,
) -> Result<PreparedState, String> {
    reopen_fixture(run_dir, family, expected_binding, CaptureFixture::Baseline)
}
pub(in super::super) fn reopen_fixture(
    run_dir: &Path,
    family: Family,
    expected_binding: Option<&PreparationBinding>,
    fixture: CaptureFixture,
) -> Result<PreparedState, String> {
    fixture.validate(family)?;
    let run_dir = run_dir.canonicalize().map_err(|error| error.to_string())?;
    let manifest_bytes = read(&run_dir.join("capture-manifest.json"))?;
    let manifest_hash = content_hash(&manifest_bytes);
    if read(&run_dir.join("capture-manifest.hash"))? != manifest_hash.as_bytes() {
        return Err("Frozen capture manifest hash changed.".into());
    }
    let manifest: CaptureManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|error| error.to_string())?;
    validate_binding(&manifest.binding)?;
    // Expanded authority comes from the caller's independently frozen definition,
    // never from a self-consistent manifest and its adjacent checksum.
    if matches!(family, Family::Expanded) && expected_binding != Some(&manifest.binding) {
        return Err("Frozen preparation differs from the expected binding.".into());
    }
    if manifest.schema_version != family.manifest_schema() || json(&manifest)? != manifest_bytes {
        return Err("Frozen capture manifest is not canonical.".into());
    }
    let source = read(&run_dir.join("source-control.ts"))?;
    let inventory = read(&run_dir.join("inventory.json"))?;
    if source != fixture.source().as_bytes()
        || content_hash(&source) != manifest.source_hash
        || content_hash(&inventory) != manifest.inventory_hash
    {
        return Err("Frozen capture artifacts changed.".into());
    }
    let decode = |name: &str, expected: &str| -> Result<StagedObligationRequest, String> {
        let bytes = read(&run_dir.join(name))?;
        if content_hash(&bytes) != expected {
            return Err("Frozen request bytes changed.".into());
        }
        let request = StagedObligationRequest::decode_with_policy(
            std::str::from_utf8(&bytes).map_err(|error| error.to_string())?,
            family.policy(),
        )
        .map_err(|error| error.to_string())?;
        if json(&request)? != bytes {
            return Err("Frozen request is not canonical.".into());
        }
        Ok(request)
    };
    let analyst_request = decode("analyst-request.json", &manifest.analyst_wire_hash)?;
    let auditor_request = decode("auditor-request.json", &manifest.auditor_wire_hash)?;
    if analyst_request.specialist_id != family.roles()[0]
        || auditor_request.specialist_id != family.roles()[1]
        || analyst_request
            .intent_hash_with_policy(family.policy())
            .map_err(|error| error.to_string())?
            != manifest.analyst_request_hash
        || auditor_request
            .intent_hash_with_policy(family.policy())
            .map_err(|error| error.to_string())?
            != manifest.auditor_request_hash
        || auditor_request.parent_id.as_deref() != Some(manifest.analyst_id.as_str())
        || auditor_request.conversation_id.as_deref() != Some(manifest.conversation_id.as_str())
        || analyst_request.expected_context_id != manifest.context_id
        || auditor_request.expected_context_id != manifest.context_id
    {
        return Err("Frozen role binding changed.".into());
    }
    let app_data = run_dir
        .join("fixture/private")
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let app = app(crate::registered_source_tests::app_state(&app_data))?;
    let state = app.state::<AppState>();
    let graph = state
        .graph
        .lock()
        .map_err(|_| "Graph store unavailable.")?
        .read_snapshot_bounded(SnapshotReadLimits::default())
        .map_err(|error| error.to_string())?;
    let graph_before_hash = content_hash(&json(&graph)?);
    if graph_before_hash != manifest.graph_before_hash {
        return Err("Captured graph changed.".into());
    }
    let scope = crate::context::coverage_scope(app.handle(), &state)?;
    if scope.len() != 1
        || scope[0].registered_source_id.as_deref() != Some(manifest.registered_source_id.as_str())
        || scope[0].config_hash != manifest.config_hash
    {
        return Err("Frozen registration/plugin basis changed.".into());
    }
    let (resolved_analyst, analyst_guard) = resolve_request(
        &app,
        family.roles()[0],
        &analyst_request.request_nonce,
        ANALYST_QUESTION,
        family,
        fixture,
    )?;
    let (mut resolved_auditor, auditor_guard) = resolve_request(
        &app,
        family.roles()[1],
        &auditor_request.request_nonce,
        AUDITOR_QUESTION,
        family,
        fixture,
    )?;
    resolved_auditor.parent_id = auditor_request.parent_id.clone();
    resolved_auditor.conversation_id = auditor_request.conversation_id.clone();
    if json(&resolved_analyst)? != json(&analyst_request)?
        || json(&resolved_auditor)? != json(&auditor_request)?
    {
        return Err("Independently resolved frozen selections changed.".into());
    }
    for request in [&analyst_request, &auditor_request] {
        if !request.obligations.obligations.iter().any(|obligation| matches!(obligation, Obligation::Inventory { target, .. } if target.inventory_hash == manifest.inventory_hash)) { return Err("Frozen inventory artifact differs from selected inventory.".into()); }
    }
    let jobs = state.jobs.lock().map_err(|_| "Job store unavailable.")?;
    let analyst = jobs
        .existing_investigation_completion(&analyst_request)
        .map_err(|error| error.to_string())?
        .ok_or("Prepared analyst task is missing.")?;
    if analyst.summary.investigation_id != manifest.analyst_id
        || analyst.summary.conversation_id != manifest.conversation_id
        || analyst.provider != manifest.provider
    {
        return Err("Stored analyst differs from frozen preparation.".into());
    }
    let auditor = jobs
        .existing_investigation_completion(&auditor_request)
        .map_err(|error| error.to_string())?;
    if auditor.as_ref().is_some_and(|task| {
        task.summary.parent_id.as_deref() != Some(manifest.analyst_id.as_str())
            || task.summary.conversation_id != manifest.conversation_id
            || task.provider != manifest.provider
    }) {
        return Err("Stored auditor differs from frozen preparation.".into());
    }
    let auditor_id = auditor.map(|task| task.summary.investigation_id);
    drop(jobs);
    Ok(PreparedState {
        app,
        app_data,
        analyst_request,
        auditor_request,
        analyst_id: manifest.analyst_id.clone(),
        auditor_id,
        conversation_id: manifest.conversation_id.clone(),
        graph_before_hash,
        manifest,
        manifest_hash,
        _guards: vec![analyst_guard, auditor_guard],
    })
}

#[cfg(test)]
#[path = "prepare_tests.rs"]
mod tests;
