//! Explicit n=1 stages. Local artifacts bind content, never operator/reviewer identity.
use super::{execution, prepare, records};
use agents::investigation::*;
use llm::LlmProvider;
use records::{ReceiptRef, Role, RoleReceipt};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};
use tauri::Manager;

const PROTOCOL: &[u8] =
    include_bytes!("../../../../../../docs/acceptance/v7-completion-control-v2.md");
const PROTOCOL_SHA256: &str = "6f584efbfa7f91a002eea4735294096a87979e4e4ffff618d92d00f09ad911e7";
const MAX_FILE: usize = 4 * 1024 * 1024;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactRef {
    sha256: String,
    byte_len: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalArtifact {
    path: PathBuf,
    content: ArtifactRef,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeIdentity {
    schema_version: u32,
    endpoint: String,
    model: String,
    model_digest: String,
    runtime_version: String,
    // Null means explicitly unset/unverified, never an invented runtime default.
    #[serde(deserialize_with = "required_option")]
    temperature: Option<f64>,
    #[serde(deserialize_with = "required_option")]
    seed: Option<i64>,
    #[serde(deserialize_with = "required_option")]
    context_length: Option<u32>,
    verified_read_only: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DefinitionBinding {
    definition: InvestigationSpecialist,
    definition_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FrozenDefinition {
    schema_version: u32,
    protocol_sha256: String,
    source_commit: String,
    source_tree: String,
    application_version: String,
    run_id: String,
    runtime: RuntimeIdentity,
    runtime_receipt: ExternalArtifact,
    rubric: ExternalArtifact,
    definitions: Vec<DefinitionBinding>,
    capture_manifest: prepare::CaptureManifest,
    capture_manifest_hash: String,
    analyst_obligations_hash: String,
    auditor_obligations_hash: String,
    delivery_pass: bool,
    sampling_status: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SealedRole {
    schema_version: u32,
    role: Role,
    definition_sha256: String,
    runtime_receipt: ReceiptRef,
    runtime_receipt_sha256: String,
    evidence: ArtifactRef,
    delivery_pass: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedEvidence {
    detail: InvestigationDetail,
    result: Option<InvestigationResult>,
    ledger: Option<InvestigationInputLedger>,
    events: Vec<InvestigationEvent>,
    mechanical_checks: Vec<records::MechanicalCheck>,
    receipt: RoleReceipt,
    observations: Vec<execution::InvocationObservation>,
    representation_reviewable: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewBinding {
    schema: String,
    protocol_sha256: String,
    run_id: String,
    role: String,
    receipt_sha256: String,
    rubric_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    schema: String,
    protocol_sha256: String,
    run_id: String,
    role: String,
    receipt_sha256: String,
    rubric_sha256: String,
    verdict: String,
    reasons: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Comment {
    database_id: u64,
    url: String,
    created_at: String,
    body_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Attestation {
    schema: String,
    binding: ReviewBinding,
    comment: Comment,
    review: Review,
}
fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}
fn is_hash(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn json(value: &impl Serialize) -> Result<Vec<u8>, String> {
    crate::investigations::context::bounded_json(value, MAX_FILE).map_err(|e| e.to_string())
}
fn reference(bytes: &[u8]) -> ArtifactRef {
    ArtifactRef {
        sha256: sha(bytes),
        byte_len: bytes.len() as u64,
    }
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    if !std::fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_file()
    {
        return Err("Artifact must be a regular file.".into());
    }
    let mut bytes = vec![];
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_FILE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FILE {
        return Err("Artifact exceeds its bound.".into());
    }
    Ok(bytes)
}
fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    serde_json::from_slice(bytes).map_err(|e| e.to_string())
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_FILE {
        return Err("Artifact exceeds its bound.".into());
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    // Portable directory sync: a plain `File::open` of a directory is
    // refused on Windows.
    super::super::expanded::vendure_prepare::sync_directory(
        path.parent().ok_or("Missing artifact parent.")?,
    )
}
fn external(path: &Path, run_dir: &Path) -> Result<ExternalArtifact, String> {
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    let run_root = if run_dir.exists() {
        run_dir.canonicalize().map_err(|e| e.to_string())?
    } else {
        run_dir
            .parent()
            .ok_or("Missing run parent.")?
            .canonicalize()
            .map_err(|e| e.to_string())?
            .join(run_dir.file_name().ok_or("Missing run name.")?)
    };
    if path.starts_with(&run_root) {
        return Err("Review/runtime materials must stay outside the run and capture roots.".into());
    }
    Ok(ExternalArtifact {
        content: reference(&read(&path)?),
        path,
    })
}
fn verify_external(artifact: &ExternalArtifact, run_dir: &Path) -> Result<Vec<u8>, String> {
    if external(&artifact.path, run_dir)? != *artifact {
        return Err("Frozen external artifact changed.".into());
    }
    read(&artifact.path)
}
fn source_identity() -> Result<(String, String), String> {
    let git = |args: &[&str]| -> Result<String, String> {
        let out = Command::new("git")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err("Git identity check failed.".into());
        }
        String::from_utf8(out.stdout).map_err(|e| e.to_string())
    };
    if !git(&["status", "--porcelain"])?.is_empty() {
        return Err("Acceptance requires a clean reviewed checkout.".into());
    }
    let commit = git(&["rev-parse", "HEAD"])?.trim().to_owned();
    let tree = git(&["rev-parse", "HEAD^{tree}"])?.trim().to_owned();
    if !is_hash(&commit, 40) || !is_hash(&tree, 40) {
        return Err("Invalid source identity.".into());
    }
    Ok((commit, tree))
}
fn definitions() -> Result<Vec<DefinitionBinding>, String> {
    [
        SpecialistId::DomainAnalystV7,
        SpecialistId::EvidenceAuditorV7,
    ]
    .into_iter()
    .map(|id| {
        let definition = id.definition().map_err(|e| e.to_string())?;
        Ok(DefinitionBinding {
            definition_sha256: sha(&json(&definition)?),
            definition,
        })
    })
    .collect()
}
fn required_option<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}
fn runtime_identity(bytes: &[u8]) -> Result<RuntimeIdentity, String> {
    let value: RuntimeIdentity = decode(bytes)?;
    if value.schema_version != 1
        || !value.verified_read_only
        || !is_hash(&value.model_digest, 64)
        || value.model.is_empty()
        || value.model.len() > 256
        || value.runtime_version.is_empty()
        || value.runtime_version.len() > 256
        || value.context_length == Some(0)
        || value.temperature.is_some_and(|t| !t.is_finite())
    {
        return Err("Invalid externally verified runtime identity receipt.".into());
    }
    // Construction checks loopback/locality only; no request or model call occurs.
    llm::OllamaProvider::new(&value.endpoint, &value.model, Duration::from_secs(180))
        .map_err(|e| e.to_string())?;
    Ok(value)
}
fn freeze(
    run_dir: &Path,
    prepared: &prepare::PreparedState,
    runtime_receipt: ExternalArtifact,
    rubric: ExternalArtifact,
    runtime: RuntimeIdentity,
) -> Result<FrozenDefinition, String> {
    let binding = &prepared.manifest.binding;
    let definition = FrozenDefinition {
        schema_version: 1,
        protocol_sha256: PROTOCOL_SHA256.into(),
        source_commit: binding.source_commit.clone(),
        source_tree: binding.source_tree.clone(),
        application_version: env!("CARGO_PKG_VERSION").into(),
        run_id: binding.run_id.clone(),
        runtime,
        runtime_receipt,
        rubric,
        definitions: definitions()?,
        capture_manifest: prepared.manifest.clone(),
        capture_manifest_hash: prepared.manifest_hash.clone(),
        analyst_obligations_hash: prepared
            .analyst_request
            .obligations
            .fingerprint()
            .map_err(|e| e.to_string())?,
        auditor_obligations_hash: prepared
            .auditor_request
            .obligations
            .fingerprint()
            .map_err(|e| e.to_string())?,
        delivery_pass: false,
        sampling_status:
            "external receipt; null sampling fields are unset or unverified; no provider overrides"
                .into(),
    };
    let bytes = json(&definition)?;
    write_new(&run_dir.join("frozen-definition.json"), &bytes)?;
    write_new(
        &run_dir.join("frozen-definition.sha256"),
        sha(&bytes).as_bytes(),
    )?;
    Ok(definition)
}
/// Capture-only stage; the returned digest must be published/frozen externally
/// before separately selecting a role. No provider is invoked here.
pub(super) fn prepare_stage(
    run_dir: &Path,
    run_id: &str,
    runtime_receipt: &Path,
    rubric: &Path,
) -> Result<FrozenDefinition, String> {
    if sha(PROTOCOL) != PROTOCOL_SHA256
        || !run_dir.is_absolute()
        || !run_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || run_id.is_empty()
        || run_id.len() > 100
        || [".", ".."].contains(&run_id)
    {
        return Err("Invalid frozen run definition.".into());
    }
    let (source_commit, source_tree) = source_identity()?;
    let parent = run_dir
        .parent()
        .ok_or("Missing run parent.")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let run_dir = parent.join(run_dir.file_name().ok_or("Missing run name.")?);
    let runtime_receipt = external(runtime_receipt, &run_dir)?;
    let rubric = external(rubric, &run_dir)?;
    let runtime = runtime_identity(&read(&runtime_receipt.path)?)?;
    std::fs::create_dir(&run_dir).map_err(|e| e.to_string())?;
    let provider =
        llm::OllamaProvider::new(&runtime.endpoint, &runtime.model, Duration::from_secs(180))
            .map_err(|e| e.to_string())?;
    let descriptor =
        crate::investigations::runtime::descriptor(&provider, InvestigationProviderMode::Local)
            .map_err(|e| e.to_string())?;
    let prepared = prepare::prepare(
        &run_dir,
        &descriptor,
        prepare::PreparationBinding {
            run_id: run_id.into(),
            protocol_hash: PROTOCOL_SHA256.into(),
            source_commit,
            source_tree,
        },
    )?;
    freeze(&run_dir, &prepared, runtime_receipt, rubric, runtime)
}
fn check_content(
    run_dir: &Path,
    expected: &str,
) -> Result<(FrozenDefinition, prepare::PreparedState), String> {
    let bytes = read(&run_dir.join("frozen-definition.json"))?;
    if !is_hash(expected, 64)
        || sha(&bytes) != expected
        || read(&run_dir.join("frozen-definition.sha256"))? != expected.as_bytes()
    {
        return Err("Published frozen definition digest changed.".into());
    }
    let definition: FrozenDefinition = decode(&bytes)?;
    let runtime_bytes = verify_external(&definition.runtime_receipt, run_dir)?;
    verify_external(&definition.rubric, run_dir)?;
    let prepared = prepare::reopen(run_dir)?;
    if definition.schema_version != 1
        || definition.delivery_pass
        || definition.sampling_status
            != "external receipt; null sampling fields are unset or unverified; no provider overrides"
        || definition.protocol_sha256 != PROTOCOL_SHA256
        || sha(PROTOCOL) != PROTOCOL_SHA256
        || definition.application_version != env!("CARGO_PKG_VERSION")
        || definition.definitions != definitions()?
        || definition.runtime != runtime_identity(&runtime_bytes)?
        || definition.capture_manifest != prepared.manifest
        || definition.capture_manifest_hash != prepared.manifest_hash
        || definition.run_id != prepared.manifest.binding.run_id
        || definition.protocol_sha256 != prepared.manifest.binding.protocol_hash
        || definition.source_commit != prepared.manifest.binding.source_commit
        || definition.source_tree != prepared.manifest.binding.source_tree
        || definition.analyst_obligations_hash
            != prepared
                .analyst_request
                .obligations
                .fingerprint()
                .map_err(|e| e.to_string())?
        || definition.auditor_obligations_hash
            != prepared
                .auditor_request
                .obligations
                .fingerprint()
                .map_err(|e| e.to_string())?
    {
        return Err("Frozen run contents changed.".into());
    }
    Ok((definition, prepared))
}
fn preflight(
    run_dir: &Path,
    expected: &str,
) -> Result<(FrozenDefinition, prepare::PreparedState), String> {
    let (definition, prepared) = check_content(run_dir, expected)?;
    let identity = source_identity()?;
    if identity
        != (
            definition.source_commit.clone(),
            definition.source_tree.clone(),
        )
    {
        return Err("Frozen implementation identity changed.".into());
    }
    Ok((definition, prepared))
}
fn role_name(role: Role) -> &'static str {
    match role {
        Role::Analyst => "analyst",
        Role::Auditor => "auditor",
    }
}
fn marker(run_dir: &Path, role: Role, expected: &str) -> Result<(), String> {
    write_new(
        &run_dir.join(format!("{}.attempt", role_name(role))),
        expected.as_bytes(),
    )
}
fn receipt_path(run_dir: &Path, role: Role) -> PathBuf {
    run_dir.join(format!("{}-receipt.json", role_name(role)))
}
fn seal(
    run_dir: &Path,
    role: Role,
    expected: &str,
    evidence: &execution::RoleEvidence,
) -> Result<SealedRole, String> {
    let name = role_name(role);
    let evidence_bytes = json(evidence)?;
    write_new(
        &run_dir.join(format!("{name}-evidence.json")),
        &evidence_bytes,
    )?;
    let inner_path = run_dir.join(format!("{name}-runtime-receipt.json"));
    let runtime_receipt =
        records::write_new_receipt(&inner_path, &evidence.receipt).map_err(|e| e.to_string())?;
    let sealed = SealedRole {
        schema_version: 1,
        role,
        definition_sha256: expected.into(),
        runtime_receipt,
        runtime_receipt_sha256: sha(&read(&inner_path)?),
        evidence: reference(&evidence_bytes),
        delivery_pass: false,
    };
    Ok(sealed)
}
fn parent_gate(
    run_dir: &Path,
    expected: &str,
    definition: &FrozenDefinition,
    prepared: &prepare::PreparedState,
    attestation_bytes: &[u8],
) -> Result<(), String> {
    if run_dir.join("analyst-invalid-run.json").exists() {
        return Err("Analyst has a preserved invalid-run record.".into());
    }
    let outer_bytes = read(&receipt_path(run_dir, Role::Analyst))?;
    let sealed: SealedRole = decode(&outer_bytes)?;
    if sealed.schema_version != 1
        || sealed.role != Role::Analyst
        || sealed.definition_sha256 != expected
        || sealed.delivery_pass
    {
        return Err("Parent seal differs from frozen run.".into());
    }
    let inner_path = run_dir.join("analyst-runtime-receipt.json");
    let receipt = records::read_verified_receipt(&inner_path, &sealed.runtime_receipt)
        .map_err(|e| e.to_string())?;
    if sha(&read(&inner_path)?) != sealed.runtime_receipt_sha256 {
        return Err("Parent runtime receipt changed.".into());
    }
    let evidence_bytes = read(&run_dir.join("analyst-evidence.json"))?;
    if reference(&evidence_bytes) != sealed.evidence {
        return Err("Parent admissible evidence changed.".into());
    }
    let saved: SavedEvidence = decode(&evidence_bytes)?;
    if saved.receipt != receipt
        || saved.mechanical_checks != receipt.mechanical_checks
        || !saved.representation_reviewable
        || saved.observations.is_empty()
        || core_prov::content_hash(&json(&saved.detail)?) != receipt.detail_hash
        || saved
            .result
            .as_ref()
            .map(|v| json(v).map(|b| core_prov::content_hash(&b)))
            .transpose()?
            != receipt.result_hash
        || core_prov::content_hash(&json(&saved.events)?) != receipt.events_hash
        || saved.ledger.as_ref().is_none_or(|ledger| {
            saved.result.as_ref().is_none_or(|result| {
                ledger.fingerprint().ok().as_deref() != Some(result.input_ledger_hash.as_str())
            })
        })
    {
        return Err("Parent evidence differs from its mechanical receipt.".into());
    }
    let state = prepared.app.state::<crate::AppState>();
    let jobs = state.jobs.lock().map_err(|_| "Job store unavailable.")?;
    let id = &prepared.analyst_id;
    if jobs.investigation(id).map_err(|e| e.to_string())? != saved.detail
        || jobs.investigation_result(id).map_err(|e| e.to_string())? != saved.result
        || jobs.investigation_ledger(id).map_err(|e| e.to_string())? != saved.ledger
    {
        return Err("Saved parent differs from fresh durable coordinator evidence.".into());
    }
    let mut events = Vec::new();
    let mut after = 0;
    loop {
        let page = jobs
            .investigation_events(id, after)
            .map_err(|e| e.to_string())?;
        events.extend(page.items);
        if events.len() > 96 {
            return Err("Parent event evidence exceeds its bound.".into());
        }
        if !page.has_more {
            break;
        }
        if page.next_sequence <= after {
            return Err("Parent event cursor did not advance.".into());
        }
        after = page.next_sequence;
    }
    if events != saved.events {
        return Err("Saved parent events differ from durable evidence.".into());
    }
    validate_gate(definition, &receipt, &sha(&outer_bytes), attestation_bytes)
}
fn validate_gate(
    definition: &FrozenDefinition,
    receipt: &RoleReceipt,
    receipt_sha256: &str,
    bytes: &[u8],
) -> Result<(), String> {
    if receipt.role != Role::Analyst
        || receipt.binding.run_id != definition.run_id
        || receipt.binding.protocol_hash != definition.protocol_sha256
        || receipt.binding.source_commit != definition.source_commit
        || receipt.binding.source_tree != definition.source_tree
        || receipt.binding.context_id != definition.capture_manifest.context_id
        || receipt.binding.request_hash != definition.capture_manifest.analyst_request_hash
        || receipt.binding.obligations_hash != definition.analyst_obligations_hash
        || receipt.investigation_id != definition.capture_manifest.analyst_id
        || !records::evaluate(receipt, records::SemanticGate::Passed)
            .map_err(|e| e.to_string())?
            .control_pass
    {
        return Err("Analyst mechanical/supported gate did not pass.".into());
    }
    validate_review_attestation(
        [
            &definition.protocol_sha256,
            &definition.run_id,
            receipt_sha256,
            &definition.rubric.content.sha256,
        ],
        652,
        bytes,
    )
}
/// Content binding only. Authentication/freshness is an external operator gate.
pub(in super::super) fn validate_review_attestation(
    expected: [&str; 4],
    issue: u64,
    bytes: &[u8],
) -> Result<(), String> {
    let attestation: Attestation = decode(bytes)?;
    let binding = attestation.binding;
    let review = attestation.review;
    let comment = attestation.comment;
    let bound = |protocol: &str, run: &str, role: &str, receipt: &str, rubric: &str| {
        protocol == expected[0]
            && run == expected[1]
            && role == "analyst"
            && receipt == expected[2]
            && rubric == expected[3]
    };
    if attestation.schema != "cartograph-v7-semantic-review-attestation@1"
        || binding.schema != "cartograph-v7-semantic-review-binding@1"
        || review.schema != "cartograph-v7-semantic-review@1"
        || !bound(
            &binding.protocol_sha256,
            &binding.run_id,
            &binding.role,
            &binding.receipt_sha256,
            &binding.rubric_sha256,
        )
        || !bound(
            &review.protocol_sha256,
            &review.run_id,
            &review.role,
            &review.receipt_sha256,
            &review.rubric_sha256,
        )
        || review.verdict != "pass"
        || review.reasons.is_empty()
        || review.reasons.len() > 12
        || review
            .reasons
            .iter()
            .any(|r| r.trim().is_empty() || r.len() > 1024)
        || review.reasons.iter().map(String::len).sum::<usize>() > 4096
        || comment.database_id == 0
        || comment.url
            != format!(
                "https://github.com/true-line/cartograph/issues/{issue}#issuecomment-{}",
                comment.database_id
            )
        || comment.created_at.is_empty()
        || comment.created_at.len() > 32
        || !is_hash(&comment.body_sha256, 64)
    {
        return Err(
            "Independent semantic gate is missing, failed, or bound to different evidence.".into(),
        );
    }
    Ok(())
}
/// Preserve captured hash observations even if durable evidence extraction fails.
fn execute_stage(
    run_dir: &Path,
    prepared: &prepare::PreparedState,
    role: Role,
    provider: Arc<dyn LlmProvider>,
) -> Result<execution::RoleEvidence, String> {
    match execution::run_role(prepared, role, provider) {
        Ok(evidence) => Ok(evidence),
        Err(error) => {
            if error.post_dispatch {
                recording_failure(run_dir, role, &error.reason, &error.observations)?;
            }
            Err(error.reason)
        }
    }
}
// Best effort only when the destination remains writable. On storage failure
// the immutable attempt marker and coordinator DB still prevent silent replay.
fn recording_failure(
    run_dir: &Path,
    role: Role,
    reason: &str,
    observations: &[execution::InvocationObservation],
) -> Result<(), String> {
    let record = json(
        &serde_json::json!({"invalid_run":true,"stage":"post_dispatch_recording",
        "failure_origin":"harness_recording","reason":reason,"observations":observations,"delivery_pass":false}),
    )?;
    write_new(&run_dir.join(format!("{}-invalid-run.json",role_name(role))),&record)
        .map_err(|error|format!("Harness recording failed ({reason}); defect record unavailable ({error}); attempt marker and coordinator database retained."))?;
    write_new(
        &run_dir.join(format!("{}-recording-observations.json", role_name(role))),
        &json(&observations)?,
    )
    .map_err(|error| {
        format!(
            "Defect record retains observations, but separate observation artifact failed: {error}"
        )
    })
}
fn seal_stage(
    run_dir: &Path,
    role: Role,
    expected: &str,
    evidence: &execution::RoleEvidence,
) -> Result<SealedRole, String> {
    match seal(run_dir, role, expected, evidence) {
        Ok(sealed) => Ok(sealed),
        Err(reason) => {
            recording_failure(run_dir, role, &reason, &evidence.observations)?;
            Err(reason)
        }
    }
}
/// External orchestration MUST freshly reverify the never-edited GitHub comment
/// immediately before selecting auditor. Edited/deleted reviews invalidate that
/// gate. Local JSON and this explicit opt-in authenticate neither person nor bot.
pub(super) fn run_stage(
    run_dir: &Path,
    expected_definition_sha256: &str,
    role: Role,
    explicit_opt_in: &str,
    attestation: Option<&Path>,
    provider: Arc<dyn LlmProvider>,
) -> Result<SealedRole, String> {
    if explicit_opt_in != role_name(role) {
        return Err(
            "Explicit single-stage operator selection is required; it is not identity proof."
                .into(),
        );
    }
    let run_dir = run_dir.canonicalize().map_err(|e| e.to_string())?;
    let (definition, prepared) = preflight(&run_dir, expected_definition_sha256)?;
    let review_bytes = match (role, attestation) {
        (Role::Auditor, Some(path)) => {
            let bytes = read(path)?;
            parent_gate(
                &run_dir,
                expected_definition_sha256,
                &definition,
                &prepared,
                &bytes,
            )?;
            Some(bytes)
        }
        (Role::Auditor, None) => {
            return Err(
                "Auditor requires freshly externally verified semantic attestation.".into(),
            );
        }
        (Role::Analyst, None) => None,
        _ => return Err("Analyst cannot consume an auditor review gate.".into()),
    };
    marker(&run_dir, role, expected_definition_sha256)?; // Never removed, even after any error.
    if let Some(bytes) = review_bytes {
        write_new(&run_dir.join("auditor-preflight-attestation.json"), &bytes)?;
    }
    let observed = execute_stage(&run_dir, &prepared, role, provider)?;
    // Preserve actual observation even if a post-call frozen-input check fails.
    let sealed = seal_stage(&run_dir, role, expected_definition_sha256, &observed)?;
    if let Err(error) = preflight(&run_dir, expected_definition_sha256) {
        write_new(
            &run_dir.join(format!("{}-invalid-run.json", role_name(role))),
            &json(&serde_json::json!({"invalid_run":true,"reason":error,"delivery_pass":false}))?,
        )?;
        return Err(error);
    }
    if let Err(reason) =
        json(&sealed).and_then(|bytes| write_new(&receipt_path(&run_dir, role), &bytes))
    {
        recording_failure(&run_dir, role, &reason, &observed.observations)?;
        return Err(reason);
    }
    Ok(sealed) // Return to orchestration; never dispatch the other role here.
}

#[test]
#[ignore = "AC-0267: requires clean reviewed source, frozen runtime/rubric and explicit single-stage operator selection; never run during development"]
fn investigation_v7_completion_control_explicit_stage() {
    let required =
        |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("Set {name}; see MT-H4-05"));
    let run_dir = PathBuf::from(required("CARTOGRAPH_V7_RUN_DIR"));
    let stage = required("CARTOGRAPH_V7_STAGE");
    if stage == "prepare" {
        let definition = prepare_stage(
            &run_dir,
            &required("CARTOGRAPH_V7_RUN_ID"),
            Path::new(&required("CARTOGRAPH_V7_RUNTIME_RECEIPT")),
            Path::new(&required("CARTOGRAPH_V7_RUBRIC")),
        )
        .unwrap();
        println!(
            "Frozen definition SHA256: {}",
            sha(&json(&definition).unwrap())
        );
        return;
    }
    let role = match stage.as_str() {
        "analyst" => Role::Analyst,
        "auditor" => Role::Auditor,
        _ => panic!("Select exactly prepare, analyst or auditor"),
    };
    let expected = required("CARTOGRAPH_V7_DEFINITION_SHA256");
    let (definition, _) = preflight(&run_dir, &expected).unwrap();
    let provider = Arc::new(
        llm::OllamaProvider::new(
            &definition.runtime.endpoint,
            &definition.runtime.model,
            Duration::from_secs(180),
        )
        .unwrap(),
    );
    let attestation = std::env::var_os("CARTOGRAPH_V7_SEMANTIC_ATTESTATION").map(PathBuf::from);
    run_stage(
        &run_dir,
        &expected,
        role,
        &required("CARTOGRAPH_V7_OPERATOR_OPT_IN"),
        attestation.as_deref(),
        provider,
    )
    .unwrap();
}
#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
