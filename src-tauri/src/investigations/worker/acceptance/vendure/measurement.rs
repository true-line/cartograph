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
    format!("{:x}", Sha256::digest(bytes))
}

/// The original bounded preparation receipt is an external binding, not a new
/// preparation. Status text is never deserialized into trusted static fields.
fn measure(
    config: &Path,
    preparation: &Path,
    proposal: &Path,
    output: &Path,
) -> Result<Vec<Measurement>, String> {
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
    let (profile, config_hash) = profile::NativeProfile::read(config)?;
    validate_paths(source_root, preparation, proposal, config, &profile, output)?;
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
    let source_id = expected["graph"]["source_id"]
        .as_str()
        .ok_or("missing source binding")?;
    verify_config(config, &config_hash)?;
    let app = profile.app()?;
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
    let recovered = report::collect(&state, &source, &input, profile.data())?;
    if serde_json::to_value(&recovered).map_err(|_| "invalid recovered preparation")?
        != expected["graph"]
    {
        return Err("prepared graph, source, capture or coverage drift".into());
    }
    let mut result = Vec::new();
    for role in selections {
        result.push(measure_role(
            app.handle(),
            &state,
            &recovered,
            role,
            &hash(&saved),
            &hash(&raw),
        )?);
    }
    let after = report::collect(&state, &source, &input, profile.data())?;
    if bytes(&recovered)? != bytes(&after)? {
        return Err("preparation changed during measurement".into());
    }
    verify_config(config, &config_hash)?;
    if input::read_bounded(preparation, 16 * 1024 * 1024)? != saved
        || input::read_bounded(proposal, MAX_SELECTION_BYTES)? != raw
    {
        return Err("external measurement bindings changed".into());
    }
    drop(guard);
    Ok(result)
}

fn envelope(obligations: Vec<Obligation>) -> ObligationEnvelope {
    ObligationEnvelope {
        schema_version: 1,
        contract: "investigation-completion-obligations@1".into(),
        obligations,
    }
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
    let measurements = measure(&config, &preparation, &proposal, &output).unwrap();
    std::fs::create_dir(&output).expect("measurement output must be new");
    let output = OutputDirectory::pin(&output).unwrap();
    output.write("measurement.json", &measurements).unwrap();
}

#[cfg(test)]
#[path = "measurement_tests.rs"]
mod tests;
