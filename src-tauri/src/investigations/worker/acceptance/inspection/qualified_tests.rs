use super::*;

struct ControlProvider {
    calls: AtomicUsize,
    skip_inventory: bool,
    unknown: bool,
}
impl ControlProvider {
    fn new(skip_inventory: bool, unknown: bool) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            skip_inventory,
            unknown,
        }
    }
}
impl LlmProvider for ControlProvider {
    fn id(&self) -> &str {
        "local-scripted"
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: false,
            chat: true,
            tool_use: false,
        }
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        Err(ProviderError::Unsupported("control embeddings"))
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        Scripted::new(Mode::Finish).bounded_profile()
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        _: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        let total = self.calls.fetch_add(1, Ordering::SeqCst);
        let call = total % if self.skip_inventory { 3 } else { 4 };
        if self.unknown {
            return Err(BoundedCallError {
                code: FailureCode::Transport,
                outcome: InvocationOutcome::Unknown,
            });
        }
        let prompt: Value = serde_json::from_str(&request.payload().prompt).unwrap();
        assert!(!request.payload().prompt.contains("PRIVATE_RUBRIC_ORACLE"));
        assert!(!request.payload().system.contains("PRIVATE_RUBRIC_ORACLE"));
        let step = if self.skip_inventory { call + 1 } else { call };
        let response = match step {
            0 => {
                let repo = &prompt["coverage_qualification"]["repos"][0];
                json!({"type":"read_coverage_inventory","context_id":prompt["context_id"],"repo":repo["repo"],"inventory_hash":repo["inventory_hash"],"max_bytes":32768})
            }
            1 => {
                json!({"type":"query_context","context_id":prompt["context_id"],"query":{"scope":{"type":"all"},"kind":null,"labels":["BusinessRule"],"max_facts":1,"max_bytes":32768,"cursor":null}})
            }
            2 => {
                let target = &prompt["inspection_requirements"][0];
                json!({"type":"read_evidence","fact":target["fact"],"role":target["role"],"index":target["index"]})
            }
            _ => {
                let citation = prompt["citations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|citation| citation["origin"]["kind"] == "captured_primary_source")
                    .unwrap();
                let history = prompt["prior_findings_as_t3_history"]
                    .as_array()
                    .is_some_and(|history| !history.is_empty());
                json!({"type":"finish","findings":[{"claim_kind":"inferred_interpretation","title":if history {"Bounded expression review"} else {"Local comparison evidence"},
                    "statement":if history {"This isolated initializer supplies a comparison, while broader execution outcomes require additional disclosed evidence."} else {"The retained expression tests a Boolean value; surrounding branch behavior needs a separate supplied range."},
                    "citation_ids":[citation["citation_id"]],"limitations":[if history {"The audit does not establish deployment behavior."} else {"Only an initializer range was supplied."}]}],"knowledge_completeness":"partial","limitations":[if history {"Parent claims remain attributed history."} else {"Coverage limits do not prove project-wide absence."}]})
            }
        };
        Ok(BoundedCompletion {
            text: response.to_string(),
            requested_model: "scripted-model".into(),
            response_model: Some("scripted-model-observed".into()),
            provider_request_id: None,
            usage: None,
            request_bytes: 100,
            response_bytes: 100,
        })
    }
}
struct Files {
    root: tempfile::TempDir,
    output: PathBuf,
    rubric: PathBuf,
    runtime: PathBuf,
    mailbox: PathBuf,
}
impl Files {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output");
        let mailbox = root.path().join("mailbox");
        std::fs::create_dir(&output).unwrap();
        std::fs::create_dir(&mailbox).unwrap();
        let rubric = root.path().join("rubric.md");
        std::fs::write(
            &rubric,
            "PRIVATE_RUBRIC_ORACLE: evaluator-only synthetic criteria.",
        )
        .unwrap();
        let runtime = root.path().join("runtime.json");
        std::fs::write(&runtime, b"{\"scripted_only\":true}").unwrap();
        Self {
            root,
            output,
            rubric,
            runtime,
            mailbox,
        }
    }
    fn prepare(
        &self,
        provider: &dyn LlmProvider,
        factory: fn(&str) -> String,
    ) -> (Fixture, Frozen) {
        prepare_with_inventory(
            provider,
            tempfile::tempdir_in(self.root.path()).unwrap(),
            &self.output,
            &self.rubric,
            &self.runtime,
            &self.mailbox,
            json!({"scripted_calibration":true}),
            factory,
        )
        .unwrap()
    }
    fn acknowledge(&self, frozen: &Frozen, hash: &str, path: Option<&Path>) {
        let published = path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.mailbox.join("frozen-manifest.json"));
        std::fs::copy(self.output.join("frozen-manifest.json"), &published).unwrap();
        write_new(
            &self.output.join("publication.json"),
            &json_bytes(
                &json!({"schema_version":1,"manifest_hash":hash,"mailbox_manifest":published}),
            ),
        )
        .unwrap();
        assert_eq!(
            frozen.manifest["mailbox_root"],
            json!(self.mailbox.canonicalize().unwrap())
        );
    }
}
fn report() -> Value {
    json!({"phase":"preparation","independent_semantic_review":"pending","delivery_gate_pass":false,"mechanical_pass":false})
}

// AC-0191/0239: no publication is no model dispatch; a wrong acknowledgement
// or external non-mailbox copy never opens the exact same-process gate.
#[test]
fn qualified_acceptance_freeze_requires_exact_mailbox_ack_before_dispatch() {
    for wrong in ["missing", "hash", "outside", "changed", "symlink"] {
        let files = Files::new();
        let provider = ControlProvider::new(false, false);
        let (fixture, frozen) = files.prepare(&provider, inventory);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(fixture.request.expected_context_id.is_some());
        assert_eq!(
            fixture.request.expected_context_id.as_deref(),
            Some(frozen.archive.proof().context_id.as_str())
        );
        if wrong == "hash" {
            files.acknowledge(&frozen, &"0".repeat(64), None);
        } else if wrong == "symlink" {
            #[cfg(unix)]
            {
                let external = files.root.path().join("external-manifest.json");
                std::fs::copy(files.output.join("frozen-manifest.json"), &external).unwrap();
                let alias = files.mailbox.join("escape.json");
                std::os::unix::fs::symlink(&external, &alias).unwrap();
                write_new(&files.output.join("publication.json"), &json_bytes(&json!({"schema_version":1,"manifest_hash":frozen.manifest_hash,"mailbox_manifest":alias}))).unwrap();
            }
            #[cfg(not(unix))]
            continue;
        } else if wrong == "outside" {
            files.acknowledge(
                &frozen,
                &frozen.manifest_hash,
                Some(&files.root.path().join("outside.json")),
            );
        } else if wrong == "changed" {
            files.acknowledge(&frozen, &frozen.manifest_hash, None);
            std::fs::write(files.output.join("questions.json"), b"changed").unwrap();
        }
        assert!(
            await_publication(
                &frozen,
                &files.output.join("publication.json"),
                Duration::ZERO
            )
            .is_err()
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        stop_before_run(&fixture);
        assert_eq!(
            fixture
                .app
                .state::<AppState>()
                .jobs
                .lock()
                .unwrap()
                .investigation(&fixture.id)
                .unwrap()
                .usage
                .model_invocations,
            0
        );
    }
}

#[test]
fn qualified_acceptance_freezes_typed_synthetic_inventory_and_independent_supplied_pair() {
    let files = Files::new();
    let provider = Arc::new(ControlProvider::new(false, false));
    let (fixture, frozen) = files.prepare(provider.as_ref(), inventory);
    let value: RepoCoverage =
        serde_json::from_slice(&std::fs::read(files.output.join("inventory.json")).unwrap())
            .unwrap();
    assert!(value.producers[0].id.contains("harness-generated"));
    assert!(
        value.producers[0]
            .stopping_reason
            .contains("synthetic control")
    );
    assert_eq!(value.producers[0].state, ProducerState::MissingInput);
    assert_eq!(value.producers[0].nodes, 0);
    files.acknowledge(&frozen, &frozen.manifest_hash, None);
    await_publication(
        &frozen,
        &files.output.join("publication.json"),
        Duration::ZERO,
    )
    .unwrap();
    let mut report = report();
    execute(&fixture, &frozen, provider.clone(), &mut report);
    assert_eq!(report["mechanical_pass"], true, "{report:#}");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 8);
    for role in ["analyst", "auditor"] {
        assert_eq!(report[role]["result"]["schema_version"], 4);
        assert_eq!(report[role]["ledger"]["schema_version"], 2);
        assert_eq!(
            report[role]["sealed_invocations"].as_array().unwrap().len(),
            4
        );
        assert_eq!(
            report[role]["checks"]["inventory_disclosed_at_finish"],
            true
        );
        assert_eq!(
            report[role]["review_evidence"]["originals_match_disclosed_body_hashes"],
            true
        );
        assert!(report[role]["detail"]["usage"]["reported_input_tokens"].is_null());
    }
    assert_eq!(
        report["auditor"]["checks"]["parent_history_supplied_when_required"],
        true
    );
    assert_eq!(report["independent_semantic_review"], "pending");
    assert_eq!(report["delivery_gate_pass"], false);
    assert!(files.output.join("auditor-preparation.json").is_file());
    // Independently pin the actual complete parent record, not merely nonempty
    // attributed history. Hash, result identity and record bytes are separate.
    let prepared: Value = serde_json::from_slice(
        &std::fs::read(files.output.join("auditor-preparation.json")).unwrap(),
    )
    .unwrap();
    let input: InvestigationInputLedger =
        serde_json::from_value(report["auditor"]["ledger"].clone()).unwrap();
    let observation = &report["auditor"]["observed_invocations"][0];
    assert!(exact_saved_parent_history(observation, &input, &prepared));
    for field in ["array_hash", "result_id", "bytes_hash"] {
        let mut changed = observation.clone();
        if field == "array_hash" {
            changed["history"][field] = json!("0".repeat(64));
        } else {
            changed["history"]["saved_results"][0][field] = json!("different-parent-record");
        }
        assert!(
            !exact_saved_parent_history(&changed, &input, &prepared),
            "{field}"
        );
    }
}

fn secret_inventory(repo: &str) -> String {
    let mut inventory: RepoCoverage = serde_json::from_str(&inventory(repo)).unwrap();
    inventory.producers[0]
        .stopping_reason
        .push_str(" password=synthetic-control-secret ");
    serde_json::to_string(&inventory).unwrap()
}
#[test]
fn qualified_acceptance_inventory_redaction_keeps_mechanical_and_review_outcomes_separate() {
    let files = Files::new();
    let provider = Arc::new(ControlProvider::new(false, false));
    let (fixture, frozen) = files.prepare(provider.as_ref(), secret_inventory);
    files.acknowledge(&frozen, &frozen.manifest_hash, None);
    await_publication(
        &frozen,
        &files.output.join("publication.json"),
        Duration::ZERO,
    )
    .unwrap();
    let mut report = report();
    execute(&fixture, &frozen, provider.clone(), &mut report);
    assert_eq!(report["mechanical_pass"], true, "{report:#}");
    for role in ["analyst", "auditor"] {
        assert_eq!(
            report[role]["review_evidence"]["originals_match_disclosed_body_hashes"],
            false
        );
        assert!(
            report[role]["review_evidence"]["status"]
                .as_str()
                .unwrap()
                .contains("unreviewable")
        );
        assert!(
            report[role]["checks"]
                .get("retained_originals_match_disclosed_body_hashes")
                .is_none()
        );
    }
}

#[test]
fn qualified_acceptance_preserves_failed_unknown_and_missing_inventory_without_auditor_retry() {
    for (skip, unknown, expected) in [(true, false, 3), (false, true, 1)] {
        let files = Files::new();
        let provider = Arc::new(ControlProvider::new(skip, unknown));
        let (fixture, frozen) = files.prepare(provider.as_ref(), inventory);
        files.acknowledge(&frozen, &frozen.manifest_hash, None);
        await_publication(
            &frozen,
            &files.output.join("publication.json"),
            Duration::ZERO,
        )
        .unwrap();
        let mut report = report();
        execute(&fixture, &frozen, provider.clone(), &mut report);
        assert_eq!(report["mechanical_pass"], false);
        assert_eq!(provider.calls.load(Ordering::SeqCst), expected);
        assert_eq!(
            report["auditor"]["status"],
            "skipped_after_failed_analyst_mechanical_checks"
        );
        if unknown {
            assert_eq!(report["analyst"]["detail"]["status"], "outcome_unknown");
            assert_eq!(
                report["analyst"]["sealed_invocations"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
        }
    }
}

#[test]
fn qualified_acceptance_pinned_context_and_auditor_revalidation_fail_before_new_calls() {
    let files = Files::new();
    let provider = Arc::new(ControlProvider::new(false, false));
    let (fixture, frozen) = files.prepare(provider.as_ref(), inventory);
    files.acknowledge(&frozen, &frozen.manifest_hash, None);
    await_publication(
        &frozen,
        &files.output.join("publication.json"),
        Duration::ZERO,
    )
    .unwrap();
    // Graph drift is rejected by actual production re-preparation against the
    // persisted final expected_graph_revision/context intent, before a call.
    fixture
        .app
        .state::<AppState>()
        .graph
        .lock()
        .unwrap()
        .put_node(&core_graph::Node {
            id: "node:drift".into(),
            label: "Function".into(),
            props: json!({}),
        })
        .unwrap();
    let mut report = report();
    execute(&fixture, &frozen, provider.clone(), &mut report);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(report["analyst"]["detail"]["status"], "failed");
    assert!(audit_request(&fixture, &frozen).is_err());
}
