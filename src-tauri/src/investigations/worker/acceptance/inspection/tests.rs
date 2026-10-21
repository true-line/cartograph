use super::*;

/// Calibration provider only: inspect the actual on-disk preparation before
/// forwarding each deterministic action through the production worker.
struct PreparedProvider {
    inner: Arc<Scripted>,
    output: PathBuf,
    auditor: bool,
}
impl LlmProvider for PreparedProvider {
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
        let report: Value =
            serde_json::from_slice(&std::fs::read(self.output.join("run.json")).unwrap()).unwrap();
        assert_eq!(
            report["intended_specialists"]["analyst"]["id"],
            "domain-analyst@5"
        );
        assert_eq!(
            report["intended_specialists"]["auditor"]["id"],
            "evidence-auditor@5"
        );
        for role in ["analyst", "auditor"] {
            let intent = &report["intended_inspections"][role];
            assert_eq!(
                intent["fixture_content_hash"],
                content_hash(ORIGINAL.as_bytes())
            );
            assert_eq!(intent["targets"][0]["role"], "definition_initializer");
            let target: InvestigationInspectionTarget =
                serde_json::from_value(intent["targets"][0].clone()).unwrap();
            target.validate().unwrap();
            assert_eq!(
                target.captured.file.digest,
                content_hash(ORIGINAL.as_bytes())
            );
            assert_eq!(
                intent["requirements_hash"],
                inspection_requirements_hash(&[target]).unwrap()
            );
        }
        if self.auditor {
            assert_eq!(report["auditor_target_revalidation"]["status"], "passed");
            assert_eq!(
                report["auditor_target_revalidation"]["target_id"],
                report["intended_inspections"]["auditor"]["targets"][0]["target_id"]
            );
        }
        let mut response = self.inner.complete_bounded(request, control)?;
        if self.auditor {
            let mut action: Value = serde_json::from_str(&response.text).unwrap();
            if action["type"] == "finish" {
                // Distinct scripted audit prose avoids replaying supplied parent
                // findings; this remains calibration, never a provider oracle.
                action["findings"][0]["title"] = json!("Review scope remains narrow");
                action["findings"][0]["statement"] = json!(
                    "The retained initializer supports a local expression observation; deployment policy remains outside this audit."
                );
                action["findings"][0]["limitations"] =
                    json!(["Independent runtime confirmation was not acquired."]);
                action["limitations"] = json!(["The audit has a single captured-source scope."]);
                response.text = serde_json::to_string(&action).unwrap();
            }
        }
        Ok(response)
    }
}

// AC-0191/0261: scripted calibration is not real-provider acceptance evidence.
#[test]
fn investigation_inspection_acceptance_records_intent_before_calls_and_supplied_pair() {
    let specialists = AcceptanceSpecialists::SOURCE_INSPECTION_V5;
    let output = tempfile::tempdir().unwrap();
    let analyst = Arc::new(Scripted::new(Mode::Finish));
    let fixture = specialists.analyst_fixture(analyst.as_ref(), tempfile::tempdir().unwrap());
    let plan = InspectionPlan::prepare(&fixture).unwrap();
    let mut report = json!({"phase":"preparation", "intended_specialists":specialists.intended_definitions(), "independent_citation_review":"pending", "delivery_gate_pass":false});
    plan.record(&mut report);
    write_report(output.path(), &report);
    assert_eq!(analyst.calls.load(Ordering::SeqCst), 0);
    fixture.run(Arc::new(PreparedProvider {
        inner: analyst.clone(),
        output: output.path().into(),
        auditor: false,
    }));
    let (evidence, passed) = selected_evidence(&fixture, &fixture.id, false, Some(&plan.analyst));
    assert!(passed);
    assert_eq!(
        evidence["detail"]["limits"],
        json!(InvestigationLimits::default())
    );
    assert_eq!(analyst.calls.load(Ordering::SeqCst), 3);
    // Deliberately change the transient inherited selection to a different valid
    // role. The auditor must resolve its own initializer from host inventory.
    let alternate = range_target(&fixture, TaskRangeRole::RuleExit).unwrap();
    let mut fixture = fixture;
    fixture.request.inspection_requirements = Some(vec![alternate.clone()]);
    assert_ne!(alternate.target_id, plan.auditor.target_id);
    let auditor = Arc::new(Scripted::new(Mode::Finish));
    let passed = record_auditor(
        &fixture,
        Arc::new(PreparedProvider {
            inner: auditor.clone(),
            output: output.path().into(),
            auditor: true,
        }),
        specialists,
        Some(&plan),
        &mut report,
        output.path(),
    );
    assert!(
        passed,
        "status={} error={} checks={} revalidation={}",
        report["auditor"]["detail"]["status"],
        report["auditor"]["detail"]["error"],
        report["auditor"]["checks"],
        report["auditor_target_revalidation"]
    );
    assert_eq!(auditor.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        report["auditor"]["detail"]["inspection_requirements"],
        json!([plan.auditor])
    );
    assert_eq!(
        report["auditor"]["checks"]["parent_history_supplied_when_required"],
        true
    );
    assert_eq!(
        report["auditor"]["detail"]["limits"],
        evidence["detail"]["limits"]
    );
    assert_eq!(report["independent_citation_review"], "pending");
    assert_eq!(report["delivery_gate_pass"], false);
}

#[test]
fn investigation_inspection_acceptance_preserves_partial_wrong_range_failed_and_unknown() {
    for (mode, role, status) in [
        (
            Mode::MetadataFinish,
            TaskRangeRole::DefinitionInitializer,
            InvestigationStatus::Completed,
        ),
        (
            Mode::Finish,
            TaskRangeRole::RuleExit,
            InvestigationStatus::Completed,
        ),
        (
            Mode::Malformed,
            TaskRangeRole::DefinitionInitializer,
            InvestigationStatus::Failed,
        ),
        (
            Mode::Unknown,
            TaskRangeRole::DefinitionInitializer,
            InvestigationStatus::OutcomeUnknown,
        ),
    ] {
        let provider = Arc::new(Scripted::new(mode));
        let fixture = Fixture::with_contract(
            provider.as_ref(),
            tempfile::tempdir().unwrap(),
            "Inspect the available guard evidence and its limits.",
            SpecialistId::DomainAnalystV5,
            Some(role),
            ORIGINAL,
        );
        let target = fixture.request.inspection_requirements.as_ref().unwrap()[0].clone();
        fixture.run(provider.clone());
        let (report, passed) = selected_evidence(&fixture, &fixture.id, false, Some(&target));
        assert!(!passed);
        assert_eq!(report["detail"]["status"], json!(status));
        assert_eq!(
            report["checks"]["inspection_exact_source_supplied_at_finish"],
            false
        );
        assert_eq!(report["independent_citation_review"], "pending");
        if matches!(mode, Mode::Finish) {
            assert_eq!(
                report["checks"]["read_completed"], true,
                "a read of another range is insufficient"
            );
        }
        if matches!(mode, Mode::Unknown) {
            assert_eq!(
                report["detail"]["usage"]["reported_input_tokens"],
                Value::Null
            );
        }
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            match mode {
                Mode::Finish => 3,
                Mode::MetadataFinish => 2,
                _ => 1,
            }
        );
        let output = tempfile::tempdir().unwrap();
        write_report(output.path(), &report);
        let saved: Value =
            serde_json::from_slice(&std::fs::read(output.path().join("run.json")).unwrap())
                .unwrap();
        assert_eq!(saved, report);
    }
}

#[test]
fn investigation_inspection_acceptance_records_stale_auditor_before_any_call() {
    let specialists = AcceptanceSpecialists::SOURCE_INSPECTION_V5;
    let provider = Arc::new(Scripted::new(Mode::Finish));
    let fixture = specialists.analyst_fixture(provider.as_ref(), tempfile::tempdir().unwrap());
    let plan = InspectionPlan::prepare(&fixture).unwrap();
    fixture.run(provider);
    fixture
        .app
        .state::<AppState>()
        .graph
        .lock()
        .unwrap()
        .put_node(&core_graph::Node {
            id: "node:control-stale-target".into(),
            label: "Function".into(),
            props: json!({}),
        })
        .unwrap();
    let output = tempfile::tempdir().unwrap();
    let auditor = Arc::new(Scripted::new(Mode::Finish));
    let mut report = json!({});
    plan.record(&mut report);
    assert!(!record_auditor(
        &fixture,
        auditor.clone(),
        specialists,
        Some(&plan),
        &mut report,
        output.path()
    ));
    assert_eq!(auditor.calls.load(Ordering::SeqCst), 0);
    assert_eq!(report["auditor_target_revalidation"]["status"], "failed");
    assert_eq!(
        report["auditor"]["status"],
        "not_started_after_target_revalidation_failure"
    );
    let saved: Value =
        serde_json::from_slice(&std::fs::read(output.path().join("run.json")).unwrap()).unwrap();
    assert_eq!(saved, report);
}

#[test]
fn investigation_inspection_acceptance_keeps_legacy_report_and_requests_unchanged() {
    for specialists in [
        AcceptanceSpecialists::V2,
        AcceptanceSpecialists::COMPACT_V3,
        AcceptanceSpecialists::TURN_V4,
    ] {
        let provider = Arc::new(Scripted::new(Mode::Finish));
        let fixture = specialists.analyst_fixture(provider.as_ref(), tempfile::tempdir().unwrap());
        assert_eq!(fixture.request.schema_version, 1);
        assert!(fixture.request.inspection_requirements.is_none());
        assert_eq!(
            fixture.request.question,
            "Inspect the available guard evidence and its limits. Query context, inspect retained source, and explain only behavior supported by citations."
        );
        fixture.run(provider);
        let original = task_evidence(&fixture, &fixture.id, false);
        let selected = selected_evidence(&fixture, &fixture.id, false, None);
        assert_eq!(
            serde_json::to_vec(&selected.0).unwrap(),
            serde_json::to_vec(&original.0).unwrap()
        );
        assert_eq!(selected.1, original.1);
        assert!(
            selected.0["checks"]
                .get("inspection_exact_source_supplied_at_finish")
                .is_none()
        );
    }
}
