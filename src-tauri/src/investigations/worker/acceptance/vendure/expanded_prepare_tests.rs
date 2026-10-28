use super::super::expanded::vendure_prepare::{Definition, Selection, prepare_bound};
use super::tests::Case;
use super::*;
use agents::investigation::*;
use llm::{Embedding, LlmProvider, Locality, ProviderCaps, ProviderError, bounded::*};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct MismatchedProvider {
    inner: Arc<dyn LlmProvider>,
    calls: AtomicUsize,
}
impl LlmProvider for MismatchedProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn locality(&self) -> Locality {
        self.inner.locality()
    }
    fn capabilities(&self) -> ProviderCaps {
        self.inner.capabilities()
    }
    fn embed(&self, batch: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        self.inner.embed(batch)
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        let mut profile = self.inner.bounded_profile()?;
        profile.requested_model.push_str("-different");
        Ok(profile)
    }
    fn complete_bounded(
        &self,
        _: &AuthorizedBoundedCompletion,
        _: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("mismatched provider must never be invoked")
    }
}
fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
// AC-0287: synthetic offline ingest exercises the captured-profile bridge;
// it is not real Vendure/provider/native acceptance evidence.
#[test]
fn expanded_captured_profile_freezes_reopens_and_recovers_lost_ack_without_execution() {
    captured_case(false, None);
}
// AC-0429: synthetic adapter mechanics only, not actual-provider or #598 acceptance.
#[test]
fn expanded_captured_local_analyst_executes_once_and_reopens_without_dispatch() {
    captured_case(true, None);
}
// AC-0432, AC-0439: explicit @11/@12 is frozen separately; historical absent-field @10 stays byte stable.
#[test]
fn captured_progress_guidance_role_is_explicit_and_reopen_bound() {
    for role in ["domain-analyst@11", "domain-analyst@12"] {
        captured_case(false, Some(role));
    }
}
fn captured_case(execute: bool, specialist: Option<&str>) {
    let case = Case::new();
    let prepared = case.prepare().unwrap();
    let base = case.root.parent().unwrap();
    let receipt = base.join("preparation.json");
    let definition_path = base.join("analyst-definition.json");
    let output = base.join("analyst-output");
    let bytes = serde_json::to_vec(&prepared).unwrap();
    std::fs::write(&receipt, &bytes).unwrap();
    let graph = serde_json::to_value(&prepared.graph).unwrap();
    let candidate = graph["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| {
            !c["receipt"]["content"]["ranges"]
                .as_array()
                .unwrap()
                .is_empty()
        })
        .unwrap();
    let expected = Definition {
        specialist_id: specialist.map(str::to_owned),
        binding: super::super::completion::prepare::PreparationBinding {
            run_id: "captured-test".into(),
            protocol_hash: "a".repeat(64),
            source_commit: "b".repeat(40),
            source_tree: "c".repeat(40),
        },
        preparation_sha256: sha(&bytes),
        provider: crate::investigations::runtime::descriptor(
            crate::investigations::worker::tests::completion_tests::expanded_scripted_provider(
                false,
            )
            .as_ref(),
            InvestigationProviderMode::Local,
        )
        .unwrap(),
        selections: vec![Selection {
            fact: serde_json::from_value(candidate["binding"]["fact"].clone()).unwrap(),
            graph_snapshot_id: prepared.graph.graph_snapshot_id.clone(),
            receipt_id: candidate["binding"]["receipt_id"].as_str().unwrap().into(),
            ordinal: 0,
        }],
        question: "What does this selected source establish?".into(),
        request_nonce: "captured-test-analyst".into(),
        scope: InvestigationScope::All,
    };
    std::fs::write(&definition_path, serde_json::to_vec(&expected).unwrap()).unwrap();
    let job_ids = || {
        crate::jobs::JobStore::open(prepared.profile.data().join("state.db"))
            .unwrap()
            .list()
            .unwrap()
            .into_iter()
            .map(|job| job.id)
            .collect::<std::collections::BTreeSet<_>>()
    };
    let baseline_jobs = job_ids(); // captured ingest already owns a durable job
    let run = |definition: &Definition| {
        prepare_bound(
            &case.config,
            &receipt,
            &definition_path,
            &output,
            definition,
            InputPolicy::OfflineFixture,
        )
    };
    if specialist.is_none() {
        let wire = serde_json::to_vec(&expected).unwrap();
        assert!(
            serde_json::to_value(&expected)
                .unwrap()
                .get("specialist_id")
                .is_none()
        );
        let restored: Definition = serde_json::from_slice(&wire).unwrap();
        assert_eq!(serde_json::to_vec(&restored).unwrap(), wire);
    }
    let manifest = run(&expected).unwrap();
    let wire = std::fs::read(output.join("analyst-request.json")).unwrap();
    let request: StagedObligationRequest = serde_json::from_slice(&wire).unwrap();
    assert_eq!(
        (
            request.schema_version,
            request.specialist_id.as_str(),
            request.limit_profile.as_str()
        ),
        (
            7,
            specialist.unwrap_or("domain-analyst@10"),
            "investigation-v2"
        )
    );
    assert!(request.obligations.obligations.iter().any(|item| matches!(item,
        Obligation::Inventory { target, .. } if target.inventory_hash == prepared.graph.coverage.inventory_hash)));
    assert_eq!(request.question, expected.question);
    assert_eq!(request.request_nonce, expected.request_nonce);
    assert_eq!(manifest["capture_id"], prepared.graph.capture_id);
    assert_eq!(manifest["source_id"], prepared.graph.source_id);
    assert_eq!(manifest["provider_calls"], 0);
    assert_eq!(run(&expected).unwrap(), manifest);
    if execute {
        use super::super::expanded::{captured_local, vendure_prepare};
        use crate::investigations::worker::tests::completion_tests::expanded_scripted_provider;
        let paths = [
            case.config.as_path(),
            receipt.as_path(),
            definition_path.as_path(),
            output.as_path(),
        ];
        let mismatched = Arc::new(MismatchedProvider {
            inner: expanded_scripted_provider(false),
            calls: AtomicUsize::new(0),
        });
        let attempt = |wrong| {
            vendure_prepare::reopen_bound(
                paths,
                &expected,
                InputPolicy::OfflineFixture,
                |app, request, id, sealed, data| {
                    let provider: std::sync::Arc<dyn llm::LlmProvider> = if wrong {
                        mismatched.clone()
                    } else {
                        expanded_scripted_provider(false)
                    };
                    app.state::<AppState>()
                        .investigations
                        .set_test_provider(InvestigationProviderMode::Local, provider);
                    captured_local::use_task(
                        (app, data),
                        request,
                        id,
                        sealed,
                        &output,
                        &expected,
                        true,
                    )
                },
            )
        };
        assert_eq!(
            attempt(true).unwrap_err(),
            "Provider differs from frozen identity."
        );
        assert_eq!(mismatched.calls.load(Ordering::SeqCst), 0);
        assert!(!output.join("analyst-attempt.json").exists());
        let terminal = attempt(false).unwrap();
        assert!(
            terminal["detail"]["usage"]["model_invocations"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(attempt(false).is_err());
        assert!(run(&expected).is_err()); // preparation is never terminal reopen
        assert_eq!(
            captured_local::run(paths, &expected, InputPolicy::OfflineFixture, false).unwrap(),
            terminal
        );
        assert_eq!(
            std::fs::read(output.join("analyst-request.json")).unwrap(),
            wire
        );
        let mut expected_jobs = baseline_jobs.clone();
        assert!(expected_jobs.insert(terminal["detail"]["job_id"].as_i64().unwrap()));
        assert_eq!(job_ids(), expected_jobs); // exactly one added analyst, no reopen/retry jobs
        std::fs::write(output.join("analyst-receipt.json"), b"{}").unwrap();
        assert!(captured_local::run(paths, &expected, InputPolicy::OfflineFixture, false).is_err());
        assert!(attempt(false).is_err());
        assert_eq!(job_ids(), expected_jobs);
        return;
    }
    std::fs::remove_file(output.join("captured-analyst.json")).unwrap(); // committed start, lost final ack
    assert_eq!(run(&expected).unwrap(), manifest);
    assert_eq!(
        std::fs::read(output.join("analyst-request.json")).unwrap(),
        wire
    );
    for field in [
        "question",
        "selection",
        "provider",
        "digest",
        "nonce",
        "binding",
        "specialist",
        "unsupported_specialist",
    ] {
        let mut changed = expected.clone();
        match field {
            "specialist" => {
                changed.specialist_id = Some(if specialist.is_some() {
                    "domain-analyst@10".into()
                } else {
                    "domain-analyst@11".into()
                })
            }
            "unsupported_specialist" => changed.specialist_id = Some("evidence-auditor@11".into()),
            "question" => changed.question.push('?'),
            "selection" => changed.selections[0].ordinal += 1,
            "provider" => changed.provider.model.push('x'),
            "digest" => changed.preparation_sha256 = "0".repeat(64),
            "binding" => changed.binding.protocol_hash = "malformed".into(),
            _ => changed.request_nonce.push('x'),
        }
        std::fs::write(&definition_path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(run(&changed).is_err(), "{field}");
    }
    std::fs::write(&definition_path, serde_json::to_vec(&expected).unwrap()).unwrap();
    std::fs::write(output.join("analyst-request.json"), b"{}").unwrap();
    assert!(run(&expected).is_err());
    std::fs::write(output.join("analyst-request.json"), wire).unwrap();
    for field in [
        "source_id",
        "capture_id",
        "graph_snapshot_id",
        "source_root",
    ] {
        let mut changed = serde_json::to_value(&prepared).unwrap();
        if field == "source_root" {
            changed[field] = "/foreign".into();
        } else {
            changed["graph"][field] = "foreign".into();
        }
        std::fs::write(&receipt, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(run(&expected).is_err(), "{field}");
    }
    std::fs::write(&receipt, bytes).unwrap();
    let second_output = base.join("preconsumed-output");
    for _ in 0..2 {
        assert!(
            prepare_bound(
                &case.config,
                &receipt,
                &definition_path,
                &second_output,
                &expected,
                InputPolicy::OfflineFixture
            )
            .is_err()
        );
    }
    std::fs::remove_file(output.join("start.json")).unwrap();
    assert!(run(&expected).is_err()); // committed task without sealed start is never adopted

    let app = prepared.profile.app().unwrap();
    let state = app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let task = jobs
        .investigation(manifest["analyst_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(task.summary.status, InvestigationStatus::Queued);
    assert_eq!(task.usage.model_invocations, 0);
    assert_eq!(
        jobs.list()
            .unwrap()
            .iter()
            .filter(|job| job.investigation_id.is_some())
            .count(),
        1
    );
    let db = rusqlite::Connection::open(prepared.profile.data().join("state.db")).unwrap();
    for table in ["investigation_invocations", "completion_attempts"] {
        assert_eq!(
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    std::fs::write(&case.config, b"{}").unwrap();
    assert!(run(&expected).is_err());
}

// AC-0429, AC-0439: offline synthetic evidence; no old database is opened or reused.
// This does NOT reproduce #598 run 7: only the canonical-query/size hypothesis
// is tested. The actual failed action is unknown; no causal host fix is claimed.
#[test]
#[ignore = "requires existing frozen 17-file packet; creates a retained fresh disposable profile"]
fn captured_v12_canonical_query_continuation_repro() {
    use super::super::expanded::{captured_local, vendure_prepare};
    use crate::investigations::worker::tests::completion_tests::expanded_query_repro_provider;
    let packet = std::path::PathBuf::from(super::super::required("CARTOGRAPH_REPRO_PACKET"));
    let template = std::path::PathBuf::from(super::super::required("CARTOGRAPH_REPRO_DEFINITION"));
    let prior: Definition = serde_json::from_slice(&std::fs::read(&template).unwrap()).unwrap();
    assert_eq!(prior.specialist_id.as_deref(), Some("domain-analyst@12"));
    assert_eq!(prior.selections.len(), 7);
    let manifest = packet.join("evaluator/vendure-cart-readiness.input.json");
    let proof = packet.join("evaluator/input-proof.json");
    let source = packet.join("source");
    let before = super::input::verify(&source, &manifest, &proof).unwrap();
    assert_eq!(before.files.len(), 17);
    let old: Value = serde_json::from_slice(
        &std::fs::read(packet.join("preparation-output/preparation.json")).unwrap(),
    )
    .unwrap();
    let mut old_receipts: Vec<_> = std::fs::read_dir(&packet)
        .unwrap()
        .map(|p| p.unwrap().path().join("analyst-receipt.json"))
        .filter(|p| p.is_file())
        .map(|p| {
            let hash = sha(&std::fs::read(&p).unwrap());
            (p, hash)
        })
        .collect();
    assert_eq!(
        old_receipts.len(),
        7,
        "all seven historical receipts retained"
    );
    for path in [
        template,
        packet.join("preparation-output/preparation.json"),
        manifest.clone(),
        proof.clone(),
    ] {
        old_receipts.push((path.clone(), sha(&std::fs::read(path).unwrap())));
    }
    let mut case = Case::new();
    case.retain();
    let base = case.config.parent().unwrap();
    // Copy only verified members; all preparation paths stay in the fresh fixture.
    for path in ["order.ts", "stock.ts"] {
        std::fs::remove_file(case.root.join(path)).unwrap();
    }
    for path in before.files.keys() {
        let target = case.root.join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(source.join(path), target).unwrap();
    }
    let fresh_manifest = base.join("frozen-input.json");
    let fresh_proof = base.join("frozen-proof.json");
    std::fs::copy(&manifest, &fresh_manifest).unwrap();
    std::fs::copy(&proof, &fresh_proof).unwrap();
    assert_eq!(
        super::input::verify(&case.root, &fresh_manifest, &fresh_proof).unwrap(),
        before
    );
    eprintln!("synthetic repro retained at {}", base.display());
    let prepared = super::prepare(
        &case.root,
        &fresh_manifest,
        &fresh_proof,
        &case.config,
        prior.binding.source_commit.clone(),
        &base.join("frozen-report"),
        true,
    )
    .unwrap();
    assert_eq!(prepared.input, before);
    let fresh = serde_json::to_value(&prepared.graph).unwrap();
    let mut expected = prior.clone();
    expected.selections = prior
        .selections
        .iter()
        .map(|selection| {
            let prior = old["graph"]["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| {
                    c["binding"]["receipt_id"] == selection.receipt_id
                        && c["binding"]["fact"] == serde_json::to_value(&selection.fact).unwrap()
                })
                .unwrap();
            let fact = serde_json::to_string(&selection.fact).unwrap().replace(
                old["graph"]["source_id"].as_str().unwrap(),
                &prepared.graph.source_id,
            );
            let matches: Vec<_> = fresh["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| {
                    c["binding"]["fact"] == serde_json::from_str::<Value>(&fact).unwrap()
                        && c["receipt"]["content"]["file"]["digest"]
                            == prior["receipt"]["content"]["file"]["digest"]
                        && c["receipt"]["content"]["ranges"]
                            == prior["receipt"]["content"]["ranges"]
                })
                .collect();
            assert_eq!(matches.len(), 1, "exact fresh selected target");
            Selection {
                fact: serde_json::from_value(matches[0]["binding"]["fact"].clone()).unwrap(),
                graph_snapshot_id: prepared.graph.graph_snapshot_id.clone(),
                receipt_id: matches[0]["binding"]["receipt_id"].as_str().unwrap().into(),
                ordinal: selection.ordinal,
            }
        })
        .collect();
    let (provider, assert_received) = expanded_query_repro_provider();
    expected.provider = crate::investigations::runtime::descriptor(
        provider.as_ref(),
        InvestigationProviderMode::Local,
    )
    .unwrap();
    expected.binding.run_id = format!(
        "synthetic-{}",
        &sha(base.to_str().unwrap().as_bytes())[..24]
    );
    expected.request_nonce = expected.binding.run_id.clone();
    let receipt = base.join("preparation.json");
    let definition = base.join("definition.json");
    let output = base.join("synthetic-output");
    let bytes = serde_json::to_vec(&prepared).unwrap();
    expected.preparation_sha256 = sha(&bytes);
    std::fs::write(&receipt, bytes).unwrap();
    std::fs::write(&definition, serde_json::to_vec(&expected).unwrap()).unwrap();
    prepare_bound(
        &case.config,
        &receipt,
        &definition,
        &output,
        &expected,
        InputPolicy::FrozenVendure,
    )
    .unwrap();
    let result = vendure_prepare::reopen_bound(
        [&case.config, &receipt, &definition, &output],
        &expected,
        InputPolicy::FrozenVendure,
        |app, request, id, manifest, data| {
            app.state::<AppState>()
                .investigations
                .set_test_provider(InvestigationProviderMode::Local, provider.clone());
            captured_local::use_task((app, data), request, id, manifest, &output, &expected, true)
        },
    )
    .unwrap();
    eprintln!("synthetic terminal: {}", result["detail"]);
    assert_eq!(
        super::input::verify(&source, &manifest, &proof).unwrap(),
        before
    );
    for (path, hash) in old_receipts {
        assert_eq!(sha(&std::fs::read(path).unwrap()), hash);
    }
    assert_eq!(result["detail"]["usage"]["model_invocations"], 2);
    assert_eq!(result["detail"]["usage"]["tool_actions"], 1);
    assert_eq!(result["ledger"]["queries"].as_array().unwrap().len(), 1);
    let ledger: InvestigationInputLedger =
        serde_json::from_value(result["ledger"].clone()).unwrap();
    let second: Value = serde_json::from_str(result["invocations"][1].as_str().unwrap()).unwrap();
    assert_received(
        &ledger,
        result["request_hash"].as_str().unwrap(),
        second["input_bytes"].as_u64().unwrap() as usize,
    );
    assert_eq!(result["detail"]["status"], "failed");
    assert_eq!(
        result["detail"]["error"],
        "Action decode refused: preflight: invalid_input."
    );
    assert!(result["result"].is_null());
}
