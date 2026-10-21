use super::*;
use agents::TaskRangeRole;

pub(super) fn local_provider_acceptance(specialists: AcceptanceSpecialists) {
    // AC-0191: a real provider chooses every action through the production loop.
    // Compiling this test or passing its mechanical checks does not pass H4/H5.
    let url = required("CARTOGRAPH_ACCEPTANCE_URL");
    let model = required("CARTOGRAPH_ACCEPTANCE_MODEL");
    let digest = required("CARTOGRAPH_ACCEPTANCE_MODEL_DIGEST");
    let runtime = required("CARTOGRAPH_ACCEPTANCE_RUNTIME");
    let output = PathBuf::from(required("CARTOGRAPH_ACCEPTANCE_OUTPUT"));
    let source_revision = source_revision();
    assert!(
        output.is_absolute(),
        "The artifact directory must be absolute"
    );
    std::fs::create_dir(&output)
        .expect("The artifact directory must be new, with an existing parent");
    let mut report = json!({
        "schema_version": 1,
        "procedure": "MT-H4-02 production-module local-provider harness",
        "application_version": env!("CARGO_PKG_VERSION"),
        "source_revision": source_revision,
        "phase": "preparation",
        "intended_specialists": specialists.intended_definitions(),
        "mechanical_pass": false,
        "operator_supplied_identity": {
            "endpoint": url, "requested_model": model, "model_digest": digest, "runtime": runtime,
            "independently_verified": false,
        },
        "fixture": { "path": "rule.ts", "content_hash": content_hash(ORIGINAL.as_bytes()), "byte_len": ORIGINAL.len() },
        "independent_citation_review": "pending",
        "native_restart_check": "pending",
        "destructive_forgetting_check": "pending",
        "delivery_gate_pass": false,
    });
    write_report(&output, &report);
    let provider =
        Arc::new(llm::OllamaProvider::new(&url, &model, Duration::from_secs(180)).unwrap());
    // This calls the real bounded Ollama implementation. No mock, retry wrapper,
    // cloud fallback, shell or model download is installed by the harness.
    let mut directory = tempfile::tempdir_in(&output).unwrap();
    directory.disable_cleanup(true);
    report["retained_fixture_directory"] = json!(directory.path());
    write_report(&output, &report);
    let fixture = specialists.analyst_fixture(provider.as_ref(), directory);
    let plan = if specialists.analyst == SpecialistId::DomainAnalystV5 {
        match InspectionPlan::prepare(&fixture) {
            Ok(plan) => {
                plan.record(&mut report);
                Some(plan)
            }
            Err(message) => {
                report["phase"] = json!("preparation_failed");
                report["preparation_error"] = json!(message);
                write_report(&output, &report);
                panic!("V5 target preparation failed; no provider call was started");
            }
        }
    } else {
        None
    };
    std::fs::write(output.join("rule.ts"), ORIGINAL).unwrap();
    report["analyst_id"] = json!(fixture.id);
    report["analyst_coordinator_start_milliseconds"] = json!(fixture.start_milliseconds);
    report["phase"] = json!("analyst_running");
    write_report(&output, &report);
    fixture.run(provider.clone());
    let (analyst, analyst_pass) = selected_evidence(
        &fixture,
        &fixture.id,
        false,
        plan.as_ref().map(|p| &p.analyst),
    );
    report["analyst"] = analyst;
    report["phase"] = json!("analyst_recorded");
    write_report(&output, &report);
    let mut auditor_pass = false;
    if analyst_pass {
        auditor_pass = record_auditor(
            &fixture,
            provider,
            specialists,
            plan.as_ref(),
            &mut report,
            &output,
        );
    } else {
        if plan.is_some() {
            report["auditor_target_revalidation"] =
                json!({"status":"not_attempted_after_failed_analyst_mechanical_checks"});
        }
        report["auditor"] = json!({ "status": "skipped_after_failed_analyst_mechanical_checks" });
    }
    report["phase"] = json!("recorded");
    report["mechanical_pass"] = json!(analyst_pass && auditor_pass);
    write_report(&output, &report);
    assert!(
        analyst_pass && auditor_pass,
        "Real-provider mechanical acceptance failed; inspect run.json and retained state. No repair/replay was attempted."
    );
}

/// Metadata-only selections made independently from the recovered inventory.
/// Neither model actions nor parent findings choose these caller obligations.
struct InspectionPlan {
    analyst: InvestigationInspectionTarget,
    auditor: InvestigationInspectionTarget,
}
impl InspectionPlan {
    fn prepare(fixture: &Fixture) -> Result<Self, String> {
        let analyst = range_target(fixture, TaskRangeRole::DefinitionInitializer)?;
        if analyst.captured.file.digest != content_hash(ORIGINAL.as_bytes())
            || analyst.captured.file.byte_len != ORIGINAL.len() as u64
            || analyst.captured.file.path != "rule.ts"
        {
            return Err(
                "Host-resolved target does not bind the unchanged ORIGINAL fixture.".into(),
            );
        }
        if fixture.request.inspection_requirements.as_deref()
            != Some(std::slice::from_ref(&analyst))
        {
            return Err("Analyst request differs from the host-resolved initializer.".into());
        }
        let auditor = range_target(fixture, TaskRangeRole::DefinitionInitializer)?;
        if auditor.captured.file != analyst.captured.file {
            return Err(
                "Independently selected auditor target differs from the pinned fixture.".into(),
            );
        }
        Ok(Self { analyst, auditor })
    }
    fn record(&self, report: &mut Value) {
        report["schema_version"] = json!(2);
        report["control_variant"] = json!("source_inspection_v5");
        let intent = |target: &InvestigationInspectionTarget| {
            json!({
                "targets": [target],
                "requirements_hash": inspection_requirements_hash(std::slice::from_ref(target)).unwrap(),
                "fixture_content_hash": content_hash(ORIGINAL.as_bytes()),
            })
        };
        report["intended_inspections"] = json!({
            "analyst": intent(&self.analyst), "auditor": intent(&self.auditor),
        });
    }
}

fn range_target(
    fixture: &Fixture,
    role: TaskRangeRole,
) -> Result<InvestigationInspectionTarget, String> {
    let state = fixture.app.state::<AppState>();
    let mut nodes = state
        .graph
        .lock()
        .unwrap()
        .nodes_with_label("BusinessRule")
        .unwrap();
    if nodes.len() != 1 {
        return Err("Control fixture must contain exactly one recovered BusinessRule.".into());
    }
    let node = nodes.pop().unwrap();
    let fact = agents::TaskFactKey::Node { id: node.id };
    let description =
        crate::primary_source::describe(&state, &crate::task_evidence::graph_fact(&fact))?;
    for range in description.ranges {
        let (target, _guard) = crate::investigations::inspection::resolve(
            &state,
            &fact,
            &description.graph_snapshot_id,
            &description.receipt_id,
            range.index,
        )?;
        if target.role == role {
            return Ok(target);
        }
    }
    Err("Control fixture lacks the requested captured range role.".into())
}

fn prepare_auditor(
    fixture: &Fixture,
    plan: &InspectionPlan,
    specialists: AcceptanceSpecialists,
) -> Result<
    (
        StartInvestigationRequest,
        Vec<crate::primary_source::RetentionGuard>,
    ),
    String,
> {
    let actual = range_target(fixture, TaskRangeRole::DefinitionInitializer)?;
    if actual != plan.auditor {
        return Err(
            "Auditor target changed after the recorded preparation; no call was started.".into(),
        );
    }
    let mut request = auditor_request(fixture, specialists);
    request.schema_version = if specialists.auditor.uses_coverage_qualification() {
        3
    } else {
        2
    };
    request.expected_graph_revision = Some(actual.graph_snapshot_id.clone());
    request.inspection_requirements = Some(vec![actual]);
    request
        .validate()
        .map_err(|_| "Auditor request failed validation.")?;
    let guards = crate::investigations::inspection::validate_targets(
        &fixture.app.state::<AppState>(),
        &request,
    )?;
    Ok((request, guards))
}

fn selected_evidence(
    fixture: &Fixture,
    id: &str,
    needs_history: bool,
    target: Option<&InvestigationInspectionTarget>,
) -> (Value, bool) {
    let (mut report, passed) = task_evidence(fixture, id, needs_history);
    let Some(target) = target else {
        return (report, passed);
    };
    let state = fixture.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(id).unwrap();
    // These saved reads validate the actual finishing disclosure and immutable
    // task requirements; the report does not trust a model's supplied flags.
    let result = jobs.investigation_result(id).unwrap();
    let ledger = jobs.investigation_ledger(id).unwrap();
    let target_matches = detail.inspection_requirements.as_deref()
        == Some(std::slice::from_ref(target))
        && detail.summary.inspection_requirement_count == Some(1);
    let assessment = result
        .as_ref()
        .and_then(|r| r.inspection_assessment.as_ref());
    let supplied = assessment.is_some_and(|a| {
        a.requirements.len() == 1
            && a.requirements[0].target == *target
            && a.requirements[0].target_id == target.target_id
            && a.requirements[0].acquired
            && a.requirements[0].supplied
            && a.requirements[0].status == InvestigationInspectionStatus::Supplied
    }) && detail.summary.inspection_status
        == Some(InvestigationInspectionStatus::Supplied);
    let bound = result
        .as_ref()
        .zip(assessment)
        .zip(ledger.as_ref())
        .is_some_and(|((r, a), l)| {
            r.schema_version == if l.schema_version == 2 { 4 } else { 3 }
                && a.requirements_hash
                    == inspection_requirements_hash(std::slice::from_ref(target)).unwrap()
                && a.input_ledger_hash == r.input_ledger_hash
                && l.fingerprint().ok().as_deref() == Some(a.input_ledger_hash.as_str())
                && r.invocation_step_id.as_deref() == Some(a.finishing_step_id.as_str())
                && r.disclosure_hash.as_deref() == Some(a.disclosure_hash.as_str())
        });
    let checks = report["checks"].as_object_mut().unwrap();
    checks.insert(
        "inspection_requirements_match_prepared_target".into(),
        json!(target_matches),
    );
    checks.insert(
        "inspection_exact_source_supplied_at_finish".into(),
        json!(supplied),
    );
    checks.insert("inspection_finishing_basis_bound".into(), json!(bound));
    let passed = checks.values().all(|v| v == &Value::Bool(true));
    report["mechanical_pass"] = json!(passed);
    (report, passed)
}

fn record_auditor(
    fixture: &Fixture,
    provider: Arc<dyn LlmProvider>,
    specialists: AcceptanceSpecialists,
    plan: Option<&InspectionPlan>,
    report: &mut Value,
    output: &Path,
) -> bool {
    let prepared = if let Some(plan) = plan {
        match prepare_auditor(fixture, plan, specialists) {
            Ok(prepared) => {
                report["auditor_target_revalidation"] =
                    json!({"status":"passed", "target_id":plan.auditor.target_id});
                Some(prepared)
            }
            Err(message) => {
                report["auditor_target_revalidation"] =
                    json!({"status":"failed", "message":message});
                report["auditor"] =
                    json!({"status":"not_started_after_target_revalidation_failure"});
                write_report(output, report);
                return false;
            }
        }
    } else {
        None
    };
    report["phase"] = json!("auditor_running");
    // Revalidation success or failure is durable before any auditor invocation.
    write_report(output, report);
    let (id, start_milliseconds) = if let Some((request, _guards)) = prepared {
        run_auditor(fixture, provider, request)
    } else {
        auditor(fixture, provider, specialists)
    };
    let (evidence, passed) = selected_evidence(fixture, &id, true, plan.map(|p| &p.auditor));
    report["auditor_id"] = json!(id);
    report["auditor_coordinator_start_milliseconds"] = json!(start_milliseconds);
    report["auditor"] = evidence;
    passed
}

#[path = "inspection/tests.rs"]
mod tests;

#[path = "inspection/qualified.rs"]
pub(super) mod qualified;
