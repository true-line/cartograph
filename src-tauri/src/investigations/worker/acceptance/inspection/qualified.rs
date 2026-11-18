//! Separately versioned synthetic @6 control; never the Vendure delivery gate.
use super::*;
use ingest::coverage::{MissingInput, ProducerCoverage, ProducerKind, ProducerState, RepoCoverage};
use std::io::{Read, Write};

const ANALYST_QUESTION: &str = "For this synthetic control, describe the recorded analysis limits and what the available source establishes. Inspect the saved inventory and retained source before answering, and distinguish coverage limits from source-supported behavior.";
const AUDITOR_QUESTION: &str = "For this synthetic control, audit the saved parent's findings against the recorded limits and retained source. Perform your own inventory and source inspection, keep parent findings attributed as history, and explain only claims supported by the evidence supplied to this invocation.";
const PREVIEW_CAPACITY: usize = 1024 * 1024; // Conservative fixture-only preview; actual worker capacity remains authoritative.
const GATE_TIMEOUT: Duration = Duration::from_secs(180);

fn inventory(repo: &str) -> String {
    serde_json::to_string(&RepoCoverage {inventory:ingest::coverage::INVENTORY_ID.into(),repo:repo.into(),commit:"workdir".into(),commit_note:None,layer_hints:vec![],
        producers:vec![ProducerCoverage {id:"harness-generated-synthetic-control@1".into(),kind:ProducerKind::Observational,
            covers:"synthetic control input".into(),state:ProducerState::MissingInput,files:0,nodes:0,edges:0,inputs:vec![],
            stopping_reason:"Harness-generated synthetic control inventory: the named control producer did not run because its optional control input was not declared. This record is not a full ingest-pipeline coverage claim.".into(),refusals:None}],
        missing_inputs:vec![MissingInput {input:"synthetic_control_input".into(),producer:"harness-generated-synthetic-control@1".into(),reason:"Optional synthetic control input was not declared for this recorded scope.".into()}],
        unsupported:vec![],limited:true}).unwrap()
}

fn read_bounded(path: &Path, max: usize) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|_| "Frozen artifact is unavailable.")?;
    if !file
        .metadata()
        .map_err(|_| "Artifact metadata unavailable.")?
        .is_file()
    {
        return Err("Artifact must be a regular file.".into());
    }
    let mut bytes = Vec::new();
    file.take((max + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Artifact read failed.")?;
    if bytes.len() > max {
        return Err("Artifact exceeds its bound.".into());
    }
    Ok(bytes)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| "Immutable artifact already exists or cannot be created.")?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| "Artifact durability failed.".into())
}
fn json_bytes(value: &impl serde::Serialize) -> Vec<u8> {
    serde_json::to_vec_pretty(value).unwrap()
}
fn unique_target(fixture: &Fixture) -> Result<InvestigationInspectionTarget, String> {
    let state = fixture.app.state::<AppState>();
    let nodes = state
        .graph
        .lock()
        .unwrap()
        .nodes_with_label("BusinessRule")
        .unwrap();
    if nodes.len() != 1 {
        return Err("Control requires exactly one BusinessRule.".into());
    }
    let fact = agents::TaskFactKey::Node {
        id: nodes[0].id.clone(),
    };
    let description =
        crate::primary_source::describe(&state, &crate::task_evidence::graph_fact(&fact))?;
    let mut targets = Vec::new();
    for range in description.ranges {
        let (target, _guard) = crate::investigations::inspection::resolve(
            &state,
            &fact,
            &description.graph_snapshot_id,
            &description.receipt_id,
            range.index,
        )?;
        if target.role == TaskRangeRole::DefinitionInitializer {
            targets.push(target);
        }
    }
    if targets.len() != 1 {
        return Err("Initializer target must be unique.".into());
    }
    let target = targets.pop().unwrap();
    if target.captured.file.digest != content_hash(ORIGINAL.as_bytes())
        || target.captured.file.byte_len != ORIGINAL.len() as u64
        || target.captured.file.path != "rule.ts"
    {
        return Err("Target differs from ORIGINAL fixture.".into());
    }
    Ok(target)
}
struct Frozen {
    output: PathBuf,
    manifest: Value,
    manifest_hash: String,
    mailbox: PathBuf,
    archive: InvestigationCoverageArchive,
    plan: InspectionPlan,
    _guards: Vec<crate::primary_source::RetentionGuard>,
}
fn prepare(
    provider: &dyn LlmProvider,
    directory: tempfile::TempDir,
    output: &Path,
    rubric: &Path,
    runtime_receipt: &Path,
    mailbox: &Path,
    source: Value,
) -> Result<(Fixture, Frozen), String> {
    prepare_with_inventory(
        provider,
        directory,
        output,
        rubric,
        runtime_receipt,
        mailbox,
        source,
        inventory,
    )
}
#[allow(clippy::too_many_arguments)]
fn prepare_with_inventory(
    provider: &dyn LlmProvider,
    directory: tempfile::TempDir,
    output: &Path,
    rubric: &Path,
    runtime_receipt: &Path,
    mailbox: &Path,
    source: Value,
    inventory_factory: fn(&str) -> String,
) -> Result<(Fixture, Frozen), String> {
    let mailbox = mailbox
        .canonicalize()
        .map_err(|_| "Mailbox directory is unavailable.")?;
    let output = output
        .canonicalize()
        .map_err(|_| "Output directory unavailable.")?;
    if !mailbox.is_dir() || output.starts_with(&mailbox) || mailbox.starts_with(&output) {
        return Err("Mailbox must be a separate directory.".into());
    }
    let output = output.as_path();
    let rubric = rubric.canonicalize().map_err(|_| "Rubric unavailable.")?;
    let runtime_receipt = runtime_receipt
        .canonicalize()
        .map_err(|_| "Runtime receipt unavailable.")?;
    let rubric = rubric.as_path();
    let runtime_receipt = runtime_receipt.as_path();
    let mut preview = None;
    let fixture = Fixture::with_prepared_contract(
        provider,
        directory,
        ANALYST_QUESTION,
        SpecialistId::DomainAnalystV6,
        Some(TaskRangeRole::DefinitionInitializer),
        ORIGINAL,
        Some(inventory_factory),
        |app, state, request| {
            let prepared =
                FrozenContext::prepare_with_app(app, state, &*request, &[], Some(PREVIEW_CAPACITY))
                    .expect("Qualified preview failed before task creation");
            let archive = prepared.coverage_archive.unwrap();
            request.expected_context_id = Some(archive.proof().context_id.clone());
            preview = Some(archive);
        },
    );
    let archive = preview.ok_or("No qualified preview was prepared.")?;
    let analyst = unique_target(&fixture)?;
    let auditor = unique_target(&fixture)?;
    if fixture.request.inspection_requirements.as_deref() != Some(std::slice::from_ref(&analyst))
        || analyst.captured.file != auditor.captured.file
    {
        return Err("Independent source targets differ from prepared intent.".into());
    }
    let plan = InspectionPlan { analyst, auditor };
    let state = fixture.app.state::<AppState>();
    let mut guards = crate::investigations::inspection::validate_targets(&state, &fixture.request)?;
    let mut independent = fixture.request.clone();
    independent.specialist_id = SpecialistId::EvidenceAuditorV6;
    independent.inspection_requirements = Some(vec![plan.auditor.clone()]);
    guards.extend(crate::investigations::inspection::validate_targets(
        &state,
        &independent,
    )?);
    // Existing lookup rechecks the exact stored client-intent hash, including the
    // expected context frozen BEFORE durable task creation.
    state
        .jobs
        .lock()
        .unwrap()
        .existing_investigation(&fixture.request)
        .map_err(|_| "Stored request intent differs.")?
        .ok_or("Stored request missing.")?;
    let scoped = crate::context::coverage_scope(fixture.app.handle(), &state)?;
    if scoped.len() != 1
        || archive.proof().qualification.repos.len() != 1
        || archive.proof().qualification.exhaustive
    {
        return Err("Synthetic fixture qualification scope is unexpected.".into());
    }
    let expected_inventory = inventory_factory(&scoped[0].repo_key);
    let qualified_repo = &archive.proof().qualification.repos[0];
    let basis = archive
        .proof()
        .basis
        .first()
        .ok_or("Synthetic recorded basis missing.")?;
    if qualified_repo.status != context_hub::coverage::CoverageStatus::Recorded
        || qualified_repo.repo != scoped[0].repo_key
        || qualified_repo.inventory_hash.as_deref()
            != Some(content_hash(expected_inventory.as_bytes()).as_str())
        || basis.config_hash != scoped[0].config_hash
        || basis.registered_source_id != scoped[0].registered_source_id
    {
        return Err("Synthetic inventory must be Recorded under the exact frozen plugin/registration basis.".into());
    }
    if !rubric.is_absolute()
        || !runtime_receipt.is_absolute()
        || rubric.starts_with(
            fixture
                ._directory
                .path()
                .canonicalize()
                .map_err(|_| "Capture root unavailable.")?,
        )
        || runtime_receipt.starts_with(
            fixture
                ._directory
                .path()
                .canonicalize()
                .map_err(|_| "Capture root unavailable.")?,
        )
    {
        return Err("Rubric/runtime receipt must be absolute and outside capture fixture.".into());
    }
    let artifact = |path: &Path, bytes: &[u8]| json!({"path":path,"blake3":content_hash(bytes),"byte_len":bytes.len()});
    let mut artifacts = Vec::new();
    let record_inventory = expected_inventory;
    let files = [
        ("source-control.ts", ORIGINAL.as_bytes().to_vec()),
        ("inventory.json", record_inventory.as_bytes().to_vec()),
        ("archive.json", json_bytes(&archive)),
        (
            "definitions.json",
            json_bytes(&AcceptanceSpecialists::QUALIFIED_V6.intended_definitions()),
        ),
        ("analyst-request.json", json_bytes(&fixture.request)),
        (
            "provider-profile.json",
            json_bytes(
                &json!({"profile":provider.bounded_profile().map_err(|_|"Provider profile invalid.")?,"completion_limits":CompletionLimits::default()}),
            ),
        ),
        (
            "targets.json",
            json_bytes(&json!({"analyst":plan.analyst,"auditor":plan.auditor})),
        ),
        (
            "questions.json",
            json_bytes(&json!({"analyst":ANALYST_QUESTION,"auditor":AUDITOR_QUESTION})),
        ),
    ];
    for (name, bytes) in files {
        let path = output.join(name);
        write_new(&path, &bytes)?;
        artifacts.push(artifact(&path, &bytes));
    }
    let rubric_bytes = read_bounded(rubric, 64 * 1024)?;
    let runtime_bytes = read_bounded(runtime_receipt, 64 * 1024)?;
    artifacts.push(artifact(rubric, &rubric_bytes));
    artifacts.push(artifact(runtime_receipt, &runtime_bytes));
    let manifest = json!({"schema_version":3,"control_variant":"qualified_targeted_v6_synthetic","source":source,
        "application_version":env!("CARGO_PKG_VERSION"),"context_id":archive.proof().context_id,"coverage_id":archive.proof().qualification.coverage_id,
        "coverage_config_hash":scoped[0].config_hash,"analyst_intent_hash":fixture.request.intent_hash().unwrap(),
        "hash_algorithm":"blake3-256","mailbox_root":mailbox,"artifacts":artifacts,"rubric":{"path":rubric,"blake3":content_hash(&rubric_bytes)},
        "runtime_identity_receipt":{"path":runtime_receipt,"blake3":content_hash(&runtime_bytes),"independently_verified_by_harness":false},
        "preview":{"archive_budget_bytes":PREVIEW_CAPACITY,"capacity_admission":"conservative preview only; actual worker rechecks current storage capacity", "same_owned_context_instance":false,"production_revalidation":"expected_context_id and full graph revision persisted before start"},
        "limits":InvestigationLimits::default(),"independent_semantic_review":"pending","delivery_gate_pass":false});
    let bytes = json_bytes(&manifest);
    let manifest_hash = content_hash(&bytes);
    write_new(&output.join("frozen-manifest.json"), &bytes)?;
    Ok((
        fixture,
        Frozen {
            output: output.into(),
            manifest,
            manifest_hash,
            mailbox,
            archive,
            plan,
            _guards: guards,
        },
    ))
}
fn validate_files(frozen: &Frozen) -> Result<(), String> {
    validate_artifacts(&frozen.output, &frozen.manifest, &frozen.manifest_hash)
}
fn validate_artifacts(
    output: &Path,
    frozen_manifest: &Value,
    manifest_hash: &str,
) -> Result<(), String> {
    let manifest = read_bounded(&output.join("frozen-manifest.json"), 64 * 1024)?;
    if content_hash(&manifest) != manifest_hash {
        return Err("Frozen manifest changed.".into());
    }
    for item in frozen_manifest["artifacts"]
        .as_array()
        .ok_or("Missing artifacts.")?
    {
        let path = Path::new(item["path"].as_str().ok_or("Invalid artifact path.")?);
        let length = item["byte_len"]
            .as_u64()
            .ok_or("Invalid artifact length.")? as usize;
        let bytes = read_bounded(path, length)?;
        if bytes.len() != length || item["blake3"] != content_hash(&bytes) {
            return Err("Frozen artifact changed.".into());
        }
    }
    Ok(())
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Publication {
    schema_version: u32,
    manifest_hash: String,
    mailbox_manifest: PathBuf,
}
fn await_publication(frozen: &Frozen, gate: &Path, timeout: Duration) -> Result<u128, String> {
    if gate.file_name() != Some(std::ffi::OsStr::new("publication.json"))
        || gate
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
            .as_deref()
            != Some(frozen.output.as_path())
    {
        return Err("Explicit gate must be OUTPUT/publication.json.".into());
    }
    let started = Instant::now();
    loop {
        if gate.exists() {
            let ack: Publication = serde_json::from_slice(&read_bounded(gate, 4096)?)
                .map_err(|_| "Publication acknowledgement is invalid.")?;
            if ack.schema_version != 1
                || ack.manifest_hash != frozen.manifest_hash
                || !ack.mailbox_manifest.is_absolute()
                || ack.mailbox_manifest.starts_with(&frozen.output)
            {
                return Err(
                    "Publication acknowledgement does not bind the exact external manifest.".into(),
                );
            }
            let published_path = ack
                .mailbox_manifest
                .canonicalize()
                .map_err(|_| "Mailbox manifest is unavailable.")?;
            if !published_path.starts_with(&frozen.mailbox)
                || published_path.starts_with(&frozen.output)
            {
                return Err("Publication must be inside the frozen mailbox directory; aliases cannot escape it.".into());
            }
            let published = read_bounded(&published_path, 64 * 1024)?;
            if content_hash(&published) != frozen.manifest_hash
                || published != json_bytes(&frozen.manifest)
            {
                return Err("Published manifest differs.".into());
            }
            validate_files(frozen)?;
            return Ok(started.elapsed().as_millis());
        }
        if started.elapsed() >= timeout {
            return Err("Publication gate timed out; no provider dispatch was attempted.".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// Hash the actual array of serialized history records, retaining no history prose.
// The ledger hashes this same string-array encoding before firewall admission.
fn history_observation(prompt: &Value) -> Result<Value, ()> {
    let history = prompt["prior_findings_as_t3_history"]
        .as_array()
        .ok_or(())?;
    let mut results = Vec::new();
    for raw in history {
        let record: Value = serde_json::from_str(raw.as_str().ok_or(())?).map_err(|_| ())?;
        if !record["saved_result"].is_null() {
            let result: InvestigationResult =
                serde_json::from_value(record["saved_result"].clone()).map_err(|_| ())?;
            results.push(json!({"result_id":result.result_id,"bytes_hash":content_hash(&serde_json::to_vec(&result).map_err(|_| ())?)}));
        }
    }
    Ok(
        json!({"supplied":!history.is_empty(),"array_hash":if history.is_empty() {None} else {Some(content_hash(&serde_json::to_vec(history).map_err(|_| ())?))},"saved_results":results}),
    )
}

fn exact_saved_parent_history(
    observation: &Value,
    input: &InvestigationInputLedger,
    prepared: &Value,
) -> bool {
    observation["history"]["supplied"] == true
        && input
            .supplied_history_hash
            .as_deref()
            .is_some_and(|hash| observation["history"]["array_hash"].as_str() == Some(hash))
        && prepared["parent_result_id"].as_str().is_some()
        && prepared["parent_result_bytes_hash"].as_str().is_some()
        && observation["history"]["saved_results"]
            .as_array()
            .is_some_and(|results| {
                results.iter().any(|result| {
                    result["result_id"] == prepared["parent_result_id"]
                        && result["bytes_hash"] == prepared["parent_result_bytes_hash"]
                })
            })
}

struct ObservedProvider {
    inner: Arc<dyn LlmProvider>,
    output: PathBuf,
    role: &'static str,
    context_id: String,
    qualification: Value,
    manifest: Value,
    manifest_hash: String,
    observations: Mutex<Vec<Value>>,
}
impl ObservedProvider {
    fn new(inner: Arc<dyn LlmProvider>, frozen: &Frozen, role: &'static str) -> Self {
        Self {
            inner,
            output: frozen.output.clone(),
            role,
            context_id: frozen.archive.proof().context_id.clone(),
            qualification: serde_json::to_value(&frozen.archive.proof().qualification).unwrap(),
            manifest: frozen.manifest.clone(),
            manifest_hash: frozen.manifest_hash.clone(),
            observations: Mutex::new(vec![]),
        }
    }
}
impl LlmProvider for ObservedProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn locality(&self) -> Locality {
        self.inner.locality()
    }
    fn capabilities(&self) -> ProviderCaps {
        self.inner.capabilities()
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        self.inner.embed(texts)
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        self.inner.bounded_profile()
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        let fail = || BoundedCallError {
            code: FailureCode::InvalidInput,
            outcome: InvocationOutcome::NotDispatched,
        };
        validate_artifacts(&self.output, &self.manifest, &self.manifest_hash)
            .map_err(|_| fail())?;
        let prompt: Value = serde_json::from_str(&request.payload().prompt).map_err(|_| fail())?;
        if prompt["schema_version"] != 5
            || prompt["context_id"] != self.context_id
            || prompt["coverage_qualification"] != self.qualification
        {
            return Err(fail());
        }
        let sources=request.payload().spans.iter().map(|span| {
            let original=usize::try_from(span.byte_start).ok().zip(usize::try_from(span.byte_end).ok()).and_then(|(start,end)|ORIGINAL.as_bytes().get(start..end));
            json!({"citation_id":span.id,"path":span.path,"byte_start":span.byte_start,"byte_end":span.byte_end,
                "disclosed_text_hash":content_hash(span.text.as_bytes()),"original_control_bytes_match":span.path=="rule.ts"&&original.is_some_and(|bytes|content_hash(bytes)==content_hash(span.text.as_bytes()))})
        }).collect::<Vec<_>>();
        let details=prompt["coverage_details"].as_array().ok_or_else(fail)?.iter().map(|detail|json!({"context_id":detail["context_id"],"repo":detail["repo"],"inventory_hash":detail["inventory_hash"],"disclosed_body_hash":detail["inventory_json"].as_str().map(|body|content_hash(body.as_bytes()))})).collect::<Vec<_>>();
        let observation = json!({"payload_hash":request.payload_hash(),"input_ledger_hash":prompt["input_ledger_hash"],"context_id":prompt["context_id"],
            "coverage_qualification":prompt["coverage_qualification"],"sources":sources,"inventory_details":details,
            "history":history_observation(&prompt).map_err(|_|fail())?,
            "raw_prompt_and_response_exported":false});
        let mut observations = self.observations.lock().map_err(|_| fail())?;
        if observations.len() >= 8 {
            return Err(fail());
        }
        write_new(
            &self.output.join(format!(
                "{}-invocation-{}.json",
                self.role,
                observations.len() + 1
            )),
            &json_bytes(&observation),
        )
        .map_err(|_| fail())?;
        observations.push(observation);
        drop(observations);
        self.inner.complete_bounded(request, control)
    }
}

fn records<T: serde::de::DeserializeOwned>(
    fixture: &Fixture,
    id: &str,
    table: &str,
    maximum: usize,
    count: usize,
) -> Result<Vec<T>, String> {
    let connection = rusqlite::Connection::open_with_flags(
        fixture.app_data.join("state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|_| "Audit database unavailable.")?;
    let (rows,bytes):(i64,i64)=connection.query_row(&format!("SELECT count(*),coalesce(sum(length(CAST(body AS BLOB))),0) FROM {table} WHERE task_id=?1"),[id],|row|Ok((row.get(0)?,row.get(1)?))).map_err(|_|"Audit count failed.")?;
    if usize::try_from(rows).map_or(true, |rows| rows > count)
        || !(0..=2 * 1024 * 1024).contains(&bytes)
    {
        return Err("Audit records exceed task bounds.".into());
    }
    let mut statement=connection.prepare(&format!("SELECT CASE WHEN typeof(body)='blob' AND length(body)<=?2 THEN body END,CASE WHEN typeof(body_hash)='text' AND length(body_hash)=64 THEN body_hash END FROM {table} WHERE task_id=?1 ORDER BY rowid")).map_err(|_|"Audit query failed.")?;
    let rows = statement
        .query_map(rusqlite::params![id, maximum as i64], |row| {
            Ok((
                row.get::<_, Option<Vec<u8>>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })
        .map_err(|_| "Audit rows unavailable.")?;
    rows.map(|row| {
        let (Some(bytes), Some(hash)) = row.map_err(|_| "Audit row invalid.")? else {
            return Err("Audit body exceeds bounds.".into());
        };
        if content_hash(&bytes) != hash {
            return Err("Audit body hash differs.".into());
        }
        serde_json::from_slice(&bytes).map_err(|_| "Audit body is not its typed record.".into())
    })
    .collect()
}
// Compare each actual supplied span with its independently sealed body hash;
// envelope hashes and inventory hashes cannot establish this correspondence.
fn observed_sources_match(observed: &Value, disclosure: &InvestigationDisclosure) -> bool {
    let Some(sources) = observed["sources"].as_array() else {
        return false;
    };
    let mut actual = std::collections::BTreeMap::new();
    for source in sources {
        let (Some(id), Some(hash)) = (
            source["citation_id"].as_str(),
            source["disclosed_text_hash"].as_str(),
        ) else {
            return false;
        };
        if actual.insert(id, hash).is_some() {
            return false;
        }
    }
    let mut expected = std::collections::BTreeMap::new();
    let mut seen = std::collections::BTreeSet::new();
    for source in &disclosure.sources {
        if !seen.insert(source.citation_id.as_str()) {
            return false;
        }
        if !source.representation.is_supplied() {
            if source.disclosed_text_hash.is_some() || source.redacted {
                return false;
            }
            continue;
        }
        let Some(hash) = source.disclosed_text_hash.as_deref() else {
            return false;
        };
        expected.insert(source.citation_id.as_str(), hash);
    }
    actual == expected
}

fn evidence(
    fixture: &Fixture,
    id: &str,
    history: bool,
    target: &InvestigationInspectionTarget,
    frozen: &Frozen,
    provider: &ObservedProvider,
) -> (Value, bool) {
    let (mut report, _) = selected_evidence(fixture, id, history, Some(target));
    let state = fixture.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(id).unwrap();
    let ledger = jobs.investigation_ledger(id).unwrap();
    let result = jobs.investigation_result(id).unwrap();
    drop(jobs);
    let audited = (|| -> Result<_, String> {
        let inputs: Vec<InvestigationInputLedger> =
            records(fixture, id, "investigation_inputs", MAX_MANIFEST_BYTES, 9)?;
        let steps: Vec<PendingInvestigationStep> = records(
            fixture,
            id,
            "investigation_invocations",
            MAX_DISCLOSURE_BYTES + 4096,
            8,
        )?;
        let archives: Vec<InvestigationCoverageArchive> = records(
            fixture,
            id,
            "investigation_coverage_archives",
            2 * 1024 * 1024,
            1,
        )?;
        for archive in &archives {
            archive
                .validate()
                .map_err(|_| "Archive validation failed.")?;
        }
        let bound = steps.iter().all(|step| {
            inputs
                .iter()
                .find(|input| {
                    input.fingerprint().ok().as_deref() == Some(step.input_ledger_hash.as_str())
                })
                .is_some_and(|input| {
                    input.schema_version == 2
                        && input.coverage.as_ref() == Some(frozen.archive.proof())
                        && step.disclosure.as_ref().is_some_and(|disclosure| {
                            disclosure.schema_version == 3
                                && disclosure.payload_hash == step.payload_hash
                                && disclosure.validate(input).is_ok()
                        })
                })
        });
        Ok((steps, inputs, archives == [frozen.archive.clone()], bound))
    })();
    let (steps, inputs, archive_equal, bound, audit_error) = match audited {
        Ok((steps, inputs, archive, bound)) => (steps, inputs, archive, bound, None),
        Err(error) => (vec![], vec![], false, false, Some(error)),
    };
    let observations = provider.observations.lock().unwrap().clone();
    let observed = steps.len() == observations.len()
        && steps.iter().zip(&observations).all(|(step, observed)| {
            observed["payload_hash"] == step.payload_hash
                && observed["input_ledger_hash"] == step.input_ledger_hash
                && observed["context_id"] == frozen.archive.proof().context_id
                && observed["coverage_qualification"]
                    == serde_json::to_value(&frozen.archive.proof().qualification).unwrap()
                && step.disclosure.as_ref().is_some_and(|disclosure| {
                    observed_sources_match(observed, disclosure)
                        && observed["inventory_details"]
                            .as_array()
                            .is_some_and(|details| {
                                details.len() == disclosure.coverage_details.len()
                                    && details.iter().zip(&disclosure.coverage_details).all(
                                        |(detail, sealed)| {
                                            detail["disclosed_body_hash"]
                                                == sealed.disclosed_body_hash
                                        },
                                    )
                            })
                })
        });
    let exact_history = if history {
        read_bounded(&frozen.output.join("auditor-preparation.json"), 128 * 1024)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|prepared| {
                !observations.is_empty()
                    && steps.iter().zip(&observations).all(|(step, observation)| {
                        inputs
                            .iter()
                            .find(|input| {
                                input.fingerprint().ok().as_deref()
                                    == Some(step.input_ledger_hash.as_str())
                            })
                            .is_some_and(|input| {
                                exact_saved_parent_history(observation, input, &prepared)
                            })
                    })
            })
    } else {
        observations
            .iter()
            .all(|observation| observation["history"]["supplied"] == false)
    };
    let finishing = result
        .as_ref()
        .and_then(|r| {
            steps
                .iter()
                .find(|step| Some(step.step_id.as_str()) == r.invocation_step_id.as_deref())
        })
        .and_then(|step| step.disclosure.as_ref());
    let inventory_disclosed = ledger
        .as_ref()
        .zip(finishing)
        .is_some_and(|(ledger, disclosure)| {
            !ledger.coverage_reads.is_empty()
                && disclosure.coverage_details.iter().any(|entry| {
                    ledger
                        .coverage_reads
                        .get(entry.read_index)
                        .is_some_and(|read| frozen.archive.read(&read.request).is_ok())
                })
        });
    let original_reviewable = observations.iter().all(|observation| {
        observation["sources"].as_array().is_some_and(|sources| {
            sources
                .iter()
                .all(|source| source["original_control_bytes_match"] == true)
        }) && observation["inventory_details"]
            .as_array()
            .is_some_and(|details| {
                details
                    .iter()
                    .all(|detail| detail["inventory_hash"] == detail["disclosed_body_hash"])
            })
    });
    let tool = report["events"].as_array().is_some_and(|events| {
        events.iter().any(|event| {
            event["kind"] == "tool_completed" && event["tool"] == "read_coverage_inventory"
        })
    });
    let checks = report["checks"].as_object_mut().unwrap();
    checks.insert("inventory_tool_completed".into(), json!(tool));
    checks.insert(
        "exact_saved_parent_history_supplied".into(),
        json!(exact_history),
    );
    checks.insert(
        "inventory_disclosed_at_finish".into(),
        json!(inventory_disclosed),
    );
    checks.insert(
        "all_sealed_invocations_bind_frozen_coverage".into(),
        json!(
            !steps.is_empty()
                && bound
                && observed
                && steps.len() == detail.usage.model_invocations as usize
        ),
    );
    checks.insert(
        "archive_equal_on_fresh_connection".into(),
        json!(archive_equal),
    );
    checks.insert(
        "saved_public_qualification_exact".into(),
        json!(
            detail
                .coverage_context
                .as_ref()
                .is_some_and(
                    |context| context.context_id == frozen.archive.proof().context_id
                        && context.coverage == frozen.archive.proof().qualification
                )
        ),
    );
    checks.insert(
        "qualified_result_and_ledger_versions".into(),
        json!(
            ledger.as_ref().is_some_and(|l| l.schema_version == 2)
                && result.as_ref().is_some_and(|r| r.schema_version == 4)
        ),
    );
    let passed = checks.values().all(|check| check == &Value::Bool(true));
    report["mechanical_pass"] = json!(passed);
    report["sealed_invocations"] = json!(steps);
    report["observed_invocations"] = json!(observations);
    report["audit_error"] = json!(audit_error);
    report["review_evidence"] = json!({"originals_match_disclosed_body_hashes":original_reviewable,"status":if original_reviewable {"retained originals usable only for matching supplied hashes; semantic review pending"} else {"affected claims unreviewable; actual changed representations were not exported"},
        "source_body_hash_domain":"citation.text_hash versus disclosure.sources.disclosed_text_hash", "inventory_body_hash_domain":"inventory_hash versus coverage_details.disclosed_body_hash", "representation_envelope_hashes_are_not_body_hashes":true});
    (report, passed)
}
fn stop_before_run(fixture: &Fixture) {
    let state = fixture.app.state::<AppState>();
    let mut jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&fixture.id).unwrap();
    jobs.advance_investigation(
        &fixture.execution,
        detail.summary.revision,
        InvestigationTransition::StopWithElapsed {
            reason: InvestigationStopReason::PreparationFailed,
            active_milliseconds: 0,
        },
    )
    .unwrap();
}
fn audit_request(
    fixture: &Fixture,
    frozen: &Frozen,
) -> Result<
    (
        StartInvestigationRequest,
        Vec<crate::primary_source::RetentionGuard>,
    ),
    String,
> {
    if unique_target(fixture)? != frozen.plan.auditor {
        return Err("Independent auditor target changed.".into());
    }
    let (mut request, guards) =
        prepare_auditor(fixture, &frozen.plan, AcceptanceSpecialists::QUALIFIED_V6)?;
    request.question = AUDITOR_QUESTION.into();
    request.expected_context_id = Some(frozen.archive.proof().context_id.clone());
    request.validate().map_err(|_| "Auditor request invalid.")?;
    let state = fixture.app.state::<AppState>();
    FrozenContext::prepare_with_app(
        fixture.app.handle(),
        &state,
        &request,
        &[],
        Some(PREVIEW_CAPACITY),
    )
    .map_err(|_| "Auditor qualification changed before dispatch.")?;
    let parent = state
        .jobs
        .lock()
        .unwrap()
        .investigation_result(&fixture.id)
        .map_err(|_| "Parent result unavailable.")?
        .ok_or("Parent result missing.")?;
    if parent.schema_version != 4
        || parent
            .coverage
            .as_ref()
            .is_none_or(|coverage| coverage.context_id != frozen.archive.proof().context_id)
    {
        return Err("Parent qualification differs.".into());
    }
    write_new(
        &frozen.output.join("auditor-preparation.json"),
        &json_bytes(
            &json!({"request":request,"request_intent_hash":request.intent_hash().unwrap(),
        "parent_result_id":parent.result_id,"parent_result_bytes_hash":content_hash(&serde_json::to_vec(&parent).unwrap()),"target_revalidation":"passed","qualification_revalidation":"passed","context_id":frozen.archive.proof().context_id}),
        ),
    )?;
    Ok((request, guards))
}
fn execute(fixture: &Fixture, frozen: &Frozen, provider: Arc<dyn LlmProvider>, report: &mut Value) {
    let analyst = Arc::new(ObservedProvider::new(provider.clone(), frozen, "analyst"));
    report["phase"] = json!("analyst_running");
    write_report(&frozen.output, report);
    fixture.run(analyst.clone());
    let (observed, passed) = evidence(
        fixture,
        &fixture.id,
        false,
        &frozen.plan.analyst,
        frozen,
        &analyst,
    );
    report["analyst"] = observed;
    report["phase"] = json!("analyst_recorded");
    write_report(&frozen.output, report);
    let mut auditor_pass = false;
    if passed {
        match audit_request(fixture, frozen) {
            Ok((request, _guards)) => {
                report["auditor_target_revalidation"] = json!({"status":"passed","qualification_status":"passed","target_id":frozen.plan.auditor.target_id});
                report["phase"] = json!("auditor_running");
                write_report(&frozen.output, report);
                let auditor = Arc::new(ObservedProvider::new(provider, frozen, "auditor"));
                let (id, start) = run_auditor(fixture, auditor.clone(), request);
                let (observed, passed) =
                    evidence(fixture, &id, true, &frozen.plan.auditor, frozen, &auditor);
                auditor_pass = passed;
                report["auditor_id"] = json!(id);
                report["auditor_coordinator_start_milliseconds"] = json!(start);
                report["auditor"] = observed;
            }
            Err(message) => {
                report["auditor_target_revalidation"] =
                    json!({"status":"failed","message":message});
                report["auditor"] = json!({"status":"not_started_after_revalidation_failure"});
            }
        }
    } else {
        report["auditor"] = json!({"status":"skipped_after_failed_analyst_mechanical_checks"});
    }
    // A last in-flight request may outlive the pre-dispatch artifact check.
    let final_artifacts = validate_files(frozen);
    report["final_frozen_artifacts"] =
        json!({"unchanged":final_artifacts.is_ok(),"error":final_artifacts.err()});
    report["phase"] = json!("recorded");
    report["mechanical_pass"] =
        json!(passed && auditor_pass && report["final_frozen_artifacts"]["unchanged"] == true);
    write_report(&frozen.output, report);
}

fn resolved_output(output: &Path, checkout: &Path) -> Result<PathBuf, String> {
    if !output.is_absolute() {
        return Err("Output must be absolute.".into());
    }
    let parent = output
        .parent()
        .ok_or("Output parent is missing.")?
        .canonicalize()
        .map_err(|_| "Output parent is unavailable.")?;
    let name = output
        .file_name()
        .ok_or("Output directory name is missing.")?;
    let resolved = parent.join(name);
    let checkout = checkout
        .canonicalize()
        .map_err(|_| "Checkout is unavailable.")?;
    if resolved.starts_with(&checkout) {
        return Err("Output must be outside the checkout.".into());
    }
    Ok(resolved)
}

pub(crate) fn local_provider_acceptance() {
    let url = required("CARTOGRAPH_ACCEPTANCE_URL");
    let model = required("CARTOGRAPH_ACCEPTANCE_MODEL");
    let digest = required("CARTOGRAPH_ACCEPTANCE_MODEL_DIGEST");
    let runtime = required("CARTOGRAPH_ACCEPTANCE_RUNTIME");
    let output = PathBuf::from(required("CARTOGRAPH_ACCEPTANCE_OUTPUT"));
    let gate = PathBuf::from(required("CARTOGRAPH_ACCEPTANCE_GATE"));
    let rubric = PathBuf::from(required("CARTOGRAPH_ACCEPTANCE_RUBRIC"));
    let receipt = PathBuf::from(required("CARTOGRAPH_ACCEPTANCE_RUNTIME_EVIDENCE"));
    let mailbox = PathBuf::from(required("CARTOGRAPH_ACCEPTANCE_MAILBOX"));
    assert_eq!(gate, output.join("publication.json"));
    let output = resolved_output(
        &output,
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap(),
    )
    .expect("Fresh output must resolve outside the checkout");
    let gate = output.join("publication.json");
    let revision = source_revision();
    let tree = std::process::Command::new("git")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["rev-parse", "HEAD^{tree}"])
        .output()
        .unwrap();
    assert!(tree.status.success());
    assert!(!gate.exists());
    std::fs::create_dir(&output).expect("Fresh output directory required");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut report = json!({"schema_version":3,"control_variant":"qualified_targeted_v6_synthetic","phase":"preparation",
        "source_revision":revision,"source_tree":String::from_utf8(tree.stdout).unwrap().trim(),"intended_specialists":AcceptanceSpecialists::QUALIFIED_V6.intended_definitions(),
        "operator_supplied_identity":{"endpoint":url,"requested_model":model,"model_digest":digest,"runtime":runtime,"independently_verified":false},
        "independent_semantic_review":"pending","native_restart_check":"pending","destructive_forgetting_check":"pending","delivery_gate_pass":false,"mechanical_pass":false});
    write_report(&output, &report);
    let provider =
        Arc::new(llm::OllamaProvider::new(&url, &model, Duration::from_secs(180)).unwrap());
    let mut directory = tempfile::tempdir_in(&output).unwrap();
    directory.disable_cleanup(true);
    report["retained_fixture_directory"] = json!(directory.path());
    write_report(&output, &report);
    let prepared = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        prepare(
            provider.as_ref(),
            directory,
            &output,
            &rubric,
            &receipt,
            &mailbox,
            json!({"revision":revision,"tree":report["source_tree"],"operator_supplied_identity":report["operator_supplied_identity"]}),
        )
    }));
    let (fixture, frozen) = match prepared {
        Ok(Ok(prepared)) => prepared,
        failed => {
            report["phase"] = json!("preparation_failed");
            report["preparation_error"] = json!(match failed {
                Ok(Err(message)) => message,
                _ => "Fixture preparation failed before invocation.".into(),
            });
            write_report(&output, &report);
            panic!("V6 preparation failed; no invocation started");
        }
    };
    report["analyst_id"] = json!(fixture.id);
    report["frozen_manifest_hash"] = json!(frozen.manifest_hash);
    report["context_id"] = json!(frozen.archive.proof().context_id);
    report["phase"] = json!("awaiting_publication");
    report["gate_timeout_seconds"] = json!(GATE_TIMEOUT.as_secs());
    write_report(&output, &report);
    match await_publication(&frozen, &gate, GATE_TIMEOUT) {
        Ok(elapsed) => report["publication_wait_milliseconds"] = json!(elapsed),
        Err(message) => {
            stop_before_run(&fixture);
            report["phase"] = json!("publication_rejected");
            report["publication_error"] = json!(message);
            report["analyst"] = task_evidence(&fixture, &fixture.id, false).0;
            write_report(&output, &report);
            panic!("Publication gate rejected; no provider call started");
        }
    }
    execute(&fixture, &frozen, provider, &mut report);
    assert_eq!(
        report["mechanical_pass"], true,
        "V6 mechanical control failed; no retry/repair was attempted. Independent semantic review remains pending."
    );
}

#[path = "qualified_tests.rs"]
mod tests;
