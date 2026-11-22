//! Measurement is not admission: no task, provider, or executable request escapes.
use super::*;
use agents::{TaskFactKey, investigation::*};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[path = "measurement_cost.rs"]
mod cost;
use cost::measure_role;

const MAX_SELECTION_BYTES: usize = 128 * 1024;
const MAX_SOURCES: usize = 64;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceSelection {
    fact: TaskFactKey,
    graph_snapshot_id: String,
    receipt_id: String,
    ordinal: usize,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleSelection {
    specialist_id: String,
    question: String,
    request_nonce: String,
    scope: InvestigationScope,
    sources: Vec<SourceSelection>,
    parent_id: Option<String>,
    conversation_id: Option<String>,
}
#[derive(Serialize)]
struct RangeCost {
    target: InvestigationInspectionTarget,
    range_bytes: u64,
    full_file_validation_bytes: u64,
}
#[derive(Serialize)]
struct Measurement {
    schema_version: u32,
    status: &'static str,
    code_revision: String,
    preparation_code_revision: String,
    code_revision_matches_preparation: bool,
    preparation_sha256: String,
    raw_selection_sha256: String,
    canonical_measurement_request_sha256: String,
    specialist_id: String,
    raw_obligation_count: usize,
    canonical_obligation_count: usize,
    raw_request_bytes: usize,
    raw_envelope_bytes: usize,
    canonical_envelope_bytes: usize,
    request_binding: &'static str,
    canonical_request_bytes: usize,
    production_normalization_error: Option<String>,
    ranges: Vec<RangeCost>,
    source_evidence_items: usize,
    source_evidence_bytes: u64,
    repeated_full_file_validation_bytes: u64,
    inventory_response_bytes: usize,
    minimum_tool_actions: usize,
    minimum_model_invocations: usize,
    current_limit_violations: Vec<&'static str>,
    executable_admission_established: bool,
    compared_limits: InvestigationLimits,
    admission_caps: Value,
    provider_calls: u32,
    tasks_created: u32,
}
fn bytes(value: &impl Serialize) -> Result<Vec<u8>, String> {
    serde_json::to_vec(value).map_err(|_| "measurement serialization failed".into())
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

#[derive(Clone, Copy)]
pub(in super::super) enum InputPolicy {
    FrozenVendure,
    NegativeCases598(super::negative_cases::NegativeCase598),
    #[cfg(test)]
    OfflineFixture,
    /// #1725 side experiment: a pinned OSS estate closure, bound only by its
    /// own manifest and proof (no frozen Vendure identity).
    #[cfg(test)]
    PinnedEstate,
}
struct CodeBinding {
    code_revision: String,
    preparation_code_revision: String,
}
fn valid_code_revision(revision: &str) -> bool {
    revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
}
impl CodeBinding {
    fn new(current: &str, preparation: &Value) -> Result<Self, String> {
        let prepared = preparation["code_revision"]
            .as_str()
            .ok_or("missing preparation code revision")?;
        if !valid_code_revision(current) || !valid_code_revision(prepared) {
            return Err("invalid measurement or preparation code revision".into());
        }
        Ok(Self {
            code_revision: current.into(),
            preparation_code_revision: prepared.into(),
        })
    }
}

/// Shared read/revalidation boundary. The callback receives no provider capability;
/// source retention remains guarded through fresh graph/config/receipt checks.
pub(in super::super) fn reopen_preparation<T>(
    config: &Path,
    preparation: &Path,
    proposal: &Path,
    output: &Path,
    input_policy: InputPolicy,
    use_preparation: impl FnOnce(
        &tauri::App<tauri::test::MockRuntime>,
        &report::GraphPreparation,
        &Value,
        &[u8],
    ) -> Result<T, String>,
) -> Result<T, String> {
    let basis = reopen_basis(config, preparation, proposal, output, input_policy)?;
    let app = basis.profile.app()?;
    // A generic callback may keep a handle beyond this call.
    app.retain_with_escaped_handles();
    use_reopen_app(&basis, &app, use_preparation)
}

pub(in super::super) struct ReopenPreparation<'a> {
    config: &'a Path,
    preparation: &'a Path,
    proposal: &'a Path,
    saved: Vec<u8>,
    expected: Value,
    proposal_bytes: Vec<u8>,
    pub(super) profile: profile::NativeProfile,
    config_hash: String,
    input: input::VerifiedInput,
}
pub(in super::super) fn reopen_basis<'a>(
    config: &'a Path,
    preparation: &'a Path,
    proposal: &'a Path,
    output: &'a Path,
    input_policy: InputPolicy,
) -> Result<ReopenPreparation<'a>, String> {
    let saved = input::read_bounded(preparation, 16 * 1024 * 1024)?;
    let expected: Value =
        serde_json::from_slice(&saved).map_err(|_| "invalid preparation receipt")?;
    if expected["schema_version"] != 1
        || expected["status"] != "captured_preparation_only"
        || expected["provider_calls"] != 0
    {
        return Err("not a preparation-only receipt".into());
    }
    let source_root = Path::new(
        expected["source_root"]
            .as_str()
            .ok_or("missing source root")?,
    );
    let proposal_bytes = input::read_bounded(proposal, MAX_SELECTION_BYTES)?;
    let (profile, config_hash) = profile::NativeProfile::read(config)?;
    validate_paths(source_root, preparation, proposal, config, &profile, output)?;
    if expected["native_config_sha256"] != config_hash
        || expected["profile"] != serde_json::to_value(&profile).map_err(|_| "invalid profile")?
    {
        return Err("preparation configuration drift".into());
    }
    // Do not let app_state's normal open-or-create behavior recreate a missing
    // prepared store. Require the complete directory map and store files first.
    profile.verify_existing()?;
    for name in [
        "graph.db",
        "state.db",
        "proposals.sqlite",
        "retained-source/captures.sqlite",
        "retained-source/receipts.sqlite",
    ] {
        let path = profile.data().join(name);
        if !path.is_file() || canonical_future(&path)? != path {
            return Err("prepared production store missing or aliased".into());
        }
    }
    let input: input::VerifiedInput = serde_json::from_value(expected["input"].clone())
        .map_err(|_| "invalid preparation input binding")?;
    match input_policy {
        InputPolicy::FrozenVendure => verify_frozen_input(&input)?,
        InputPolicy::NegativeCases598(case) => case.verify(&input)?,
        InputPolicy::OfflineFixture => {}
        // Bound by the closure's own manifest and proof at preparation (#1725).
        InputPolicy::PinnedEstate => {}
    }
    expected["graph"]["source_id"]
        .as_str()
        .ok_or("missing source binding")?;
    verify_config(config, &config_hash)?;
    Ok(ReopenPreparation {
        config,
        preparation,
        proposal,
        saved,
        expected,
        proposal_bytes,
        profile,
        config_hash,
        input,
    })
}
pub(in super::super) fn use_reopen_app<T>(
    basis: &ReopenPreparation<'_>,
    app: &tauri::App<tauri::test::MockRuntime>,
    use_preparation: impl FnOnce(
        &tauri::App<tauri::test::MockRuntime>,
        &report::GraphPreparation,
        &Value,
        &[u8],
    ) -> Result<T, String>,
) -> Result<T, String> {
    let ReopenPreparation {
        config,
        preparation,
        proposal,
        saved,
        expected,
        proposal_bytes,
        profile,
        config_hash,
        input,
    } = basis;
    let source_root = Path::new(
        expected["source_root"]
            .as_str()
            .ok_or("missing source root")?,
    );
    let source_id = expected["graph"]["source_id"]
        .as_str()
        .ok_or("missing source binding")?;
    let state = app.state::<AppState>();
    let source = state
        .sources
        .lock()
        .map_err(|_| "sources unavailable")?
        .get_by_id(source_id)
        .map_err(|_| "sources unavailable")?
        .ok_or("prepared source missing")?;
    if source.root() != source_root {
        return Err("prepared registered source root drift".into());
    }
    let guard = state
        .primary_sources
        .task_guard(source_id)
        .map_err(|_| "source guard unavailable")?;
    let recovered = report::collect(&state, &source, input, profile.data())?;
    if serde_json::to_value(&recovered).map_err(|_| "invalid recovered preparation")?
        != expected["graph"]
    {
        return Err("prepared graph, source, capture or coverage drift".into());
    }
    let result = use_preparation(app, &recovered, expected, saved)?;
    let after = report::collect(&state, &source, input, profile.data())?;
    if bytes(&recovered)? != bytes(&after)? {
        return Err("preparation changed during measurement".into());
    }
    verify_config(config, config_hash)?;
    if input::read_bounded(preparation, 16 * 1024 * 1024)? != *saved
        || input::read_bounded(proposal, MAX_SELECTION_BYTES)? != *proposal_bytes
    {
        return Err("external measurement bindings changed".into());
    }
    drop(guard);
    Ok(result)
}

fn measure_bound(
    config: &Path,
    preparation: &Path,
    proposal: &Path,
    output: &Path,
    code_revision: &str,
    input_policy: InputPolicy,
) -> Result<Vec<Measurement>, String> {
    let raw = input::read_bounded(proposal, MAX_SELECTION_BYTES)?;
    let selections: Vec<RoleSelection> = agents::investigation::decode_record(
        std::str::from_utf8(&raw).map_err(|_| "invalid proposal")?,
    )
    .map_err(|_| "invalid bounded proposal")?;
    if selections.len() != 2
        || selections[0].specialist_id != "domain-analyst@7"
        || selections[1].specialist_id != "evidence-auditor@7"
        || selections
            .iter()
            .any(|role| role.sources.is_empty() || role.sources.len() > MAX_SOURCES)
    {
        return Err("explicit bounded analyst and auditor selections required".into());
    }
    // Preserve measurement's code-identity refusal before frozen-input admission;
    // the callback rechecks the helper's fresh receipt rather than trusting this read.
    let expected: Value =
        serde_json::from_slice(&input::read_bounded(preparation, 16 * 1024 * 1024)?)
            .map_err(|_| "invalid preparation receipt")?;
    CodeBinding::new(code_revision, &expected)?;
    reopen_preparation(
        config,
        preparation,
        proposal,
        output,
        input_policy,
        |app, recovered, expected, saved| {
            let code = CodeBinding::new(code_revision, expected)?;
            let state = app.state::<AppState>();
            let mut result = Vec::new();
            for role in selections {
                result.push(measure_role(
                    app.handle(),
                    &state,
                    recovered,
                    role,
                    &hash(saved),
                    &hash(&raw),
                    &code,
                )?);
            }
            if input::read_bounded(proposal, MAX_SELECTION_BYTES)? != raw {
                return Err("external measurement bindings changed".into());
            }
            Ok(result)
        },
    )
}

fn envelope(obligations: Vec<Obligation>) -> ObligationEnvelope {
    ObligationEnvelope {
        schema_version: 1,
        contract: "investigation-completion-obligations@1".into(),
        obligations,
    }
}

/// Pin a fresh external output before reading production stores. Both receipt
/// paths recheck the same evaluator/configuration boundaries before writing.
fn run_measurement(
    config: &Path,
    preparation: &Path,
    proposal: &Path,
    output: &Path,
    code_revision: &str,
    input_policy: InputPolicy,
) -> Result<(), String> {
    let saved = input::read_bounded(preparation, 16 * 1024 * 1024)?;
    let expected: Value =
        serde_json::from_slice(&saved).map_err(|_| "invalid preparation receipt")?;
    let source = Path::new(
        expected["source_root"]
            .as_str()
            .ok_or("missing source root")?,
    );
    let (profile, config_hash) = profile::NativeProfile::read(config)?;
    validate_paths(source, preparation, proposal, config, &profile, output)?;
    profile.verify_existing()?;
    let raw = input::read_bounded(proposal, MAX_SELECTION_BYTES)?;
    std::fs::create_dir(output).map_err(|_| "measurement output must be new")?;
    let pinned = OutputDirectory::pin(output)?;
    let result = measure_bound(
        config,
        preparation,
        proposal,
        output,
        code_revision,
        input_policy,
    );
    validate_paths(source, preparation, proposal, config, &profile, output)?;
    verify_config(config, &config_hash)?;
    if input::read_bounded(preparation, 16 * 1024 * 1024)? != saved
        || input::read_bounded(proposal, MAX_SELECTION_BYTES)? != raw
    {
        return Err("external measurement bindings changed".into());
    }
    match result {
        Ok(measurements) => pinned.write("measurement.json", &measurements),
        Err(reason) => {
            pinned.write(
                "measurement-failed.json",
                &serde_json::json!({
                    "schema_version":1,"status":"measurement_failed","code_revision":Some(code_revision).filter(|revision| valid_code_revision(revision)),
                    "preparation_code_revision":expected["code_revision"].as_str().filter(|revision| valid_code_revision(revision)),
                    "code_revision_matches_preparation":expected["code_revision"].as_str() == Some(code_revision),
                    "preparation_sha256":hash(&saved),"raw_selection_sha256":hash(&raw),
                    "provider_calls":0,"tasks_created":0,"executable_admission_established":false,
                    "reason":reason
                }),
            )?;
            Err(reason)
        }
    }
}

/// This fixed wrapper is the sole manual path: fixtures cannot opt out of the
/// frozen Vendure identity gate through operator environment or selection JSON.
fn run_frozen_measurement(
    config: &Path,
    preparation: &Path,
    proposal: &Path,
    output: &Path,
    code_revision: &str,
) -> Result<(), String> {
    run_measurement(
        config,
        preparation,
        proposal,
        output,
        code_revision,
        InputPolicy::FrozenVendure,
    )
}

/// Explicit owner-authorized metadata measurement; never dispatches a role.
#[test]
#[ignore = "manual prepared Vendure measurement; no task or provider"]
fn vendure_measure_prepared_selections() {
    let required = |name| std::path::PathBuf::from(super::super::required(name));
    let config = required("CARTOGRAPH_VENDURE_NATIVE_CONFIG");
    let preparation = required("CARTOGRAPH_VENDURE_PREPARATION_RECEIPT");
    let proposal = required("CARTOGRAPH_VENDURE_SELECTIONS");
    let output = required("CARTOGRAPH_VENDURE_MEASUREMENT_OUTPUT");
    let revision = super::super::source_revision();
    run_frozen_measurement(&config, &preparation, &proposal, &output, &revision).unwrap();
}

#[cfg(test)]
#[path = "measurement_tests.rs"]
mod tests;
