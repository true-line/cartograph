// ADR-0072 slice 1 (AC-1066): pure reconciliation types and procedures.
use super::*;
use crate::prepared::tests::{fixture, result};
use crate::{AgentBroker, AgentCandidate, AgentEvidence, AgentTask, ProposalStore, RawProposal};
use core_prov::{ConfidenceTier, EvidenceRef};
use serde_json::json;

fn v1_task() -> AgentTask {
    let evidence = |id: &str, path: &str, text: &str| AgentEvidence {
        id: id.into(),
        source: EvidenceRef {
            repo: "acme/cart".into(),
            path: path.into(),
            byte_start: 0,
            byte_end: text.len() as u64,
            commit_sha: "a".repeat(40),
        },
        text: text.into(),
    };
    AgentTask {
        action_id: "escalate:cart".into(),
        gap_id: "gap:cart-call".into(),
        source_id: "symbol:cart".into(),
        edge_label: "CALLS".into(),
        existing_confidence: ConfidenceTier::Gap,
        source_evidence_ids: vec!["source".into()],
        evidence: vec![
            evidence("source", "cart.ts", "cart source text"),
            evidence("target", "validate.ts", "validate target text"),
        ],
        candidates: vec![AgentCandidate {
            node_id: "symbol:validate".into(),
            label: "Symbol".into(),
            summary: "Symbol: validate".into(),
            evidence_ids: vec!["target".into()],
        }],
    }
}

fn v1_record() -> StagedProposal {
    let task = v1_task();
    let broker = AgentBroker::bounded_default();
    let proposal = broker
        .validate_response(
            &task,
            RawProposal {
                target_id: "symbol:validate".into(),
                annotation: "Possible call target".into(),
                citations: vec!["source".into(), "target".into()],
            },
        )
        .unwrap();
    ProposalStore::open(":memory:")
        .unwrap()
        .stage(
            &task,
            &proposal,
            1,
            &format!("context-v1:{}", "1".repeat(64)),
        )
        .unwrap()
}

fn v2_record() -> StagedProposal {
    let task = fixture();
    ProposalStore::open(":memory:")
        .unwrap()
        .stage_prepared(
            &task,
            &result(&task, "Possible target link", &["source", "target"]),
            1,
            &task.source_basis().graph_snapshot_id,
        )
        .unwrap()
}

fn recovery() -> RecoveryBasis {
    RecoveryBasis {
        job_id: 7,
        graph_snapshot_id: format!("context-v1:{}", "2".repeat(64)),
        captures: vec![RecoveryCapture {
            source_id: "src_registered".into(),
            capture_id: format!("capture-v1:{}", "3".repeat(64)),
        }],
    }
}

fn present_slot() -> SlotReconciliation {
    SlotReconciliation {
        source: SlotPresence::Present,
        gap: GapPresence::Present,
        relation: SlotPresence::Present,
    }
}

fn origin(path: &str, len: usize) -> ReconciledOrigin {
    ReconciledOrigin {
        receipt_id: format!("ts-primary-v3:{}", "4".repeat(64)),
        receipt_inventory_index: 0,
        captured: TaskCaptureSpanRef {
            file: crate::TaskCaptureFileRef {
                source_id: "src_registered".into(),
                capture_id: format!("capture-v1:{}", "3".repeat(64)),
                path: path.into(),
                digest: "5".repeat(64),
                byte_len: 4096,
            },
            byte_start: 0,
            byte_end: len as u64,
        },
        input_closure: InputClosureStatus::InputClosureNotEstablished,
    }
}

fn range<'a>(path: &str, digest: &'a str, bytes: &'a [u8]) -> RecapturedItem<'a> {
    RecapturedItem::Range {
        origin: origin(path, bytes.len()),
        fact_digest: digest,
        bytes,
    }
}

fn candidate(label: &str, summary: &str) -> RecapturedCandidate {
    RecapturedCandidate::Present {
        label: label.into(),
        summary: summary.into(),
    }
}

fn digest() -> String {
    format!("node-v1:{}", "d".repeat(64))
}

#[test]
fn candidate_summary_v1_is_label_then_name_path_or_id() {
    assert_eq!(
        candidate_summary_v1(
            "Symbol",
            &json!({"name": "validate", "path": "x.ts"}),
            "symbol:v"
        ),
        "Symbol: validate"
    );
    assert_eq!(
        candidate_summary_v1("File", &json!({"path": "src/x.ts"}), "file:x"),
        "File: src/x.ts"
    );
    assert_eq!(
        candidate_summary_v1("Endpoint", &json!({}), "endpoint:1"),
        "Endpoint: endpoint:1"
    );
}

#[test]
fn legacy_text_v1_hash_needs_strict_utf8() {
    assert_eq!(legacy_text_v1_hash(b"abc"), Some(content_hash(b"abc")));
    assert_eq!(
        legacy_text_v1_hash(b"caf\xc3\xa9"),
        Some(content_hash("café".as_bytes()))
    );
    // Lossy decoding would map both to U+FFFD; a lossy hash is never a match.
    assert_eq!(legacy_text_v1_hash(b"\xff"), None);
    assert_eq!(legacy_text_v1_hash(b"caf\xc3"), None);
}

#[test]
fn a_matching_v1_record_is_corroborated_never_retained() {
    let record = v1_record();
    let result = ReconciliationResultV1::derive(
        &record,
        ReconciliationInput {
            recovery: recovery(),
            slot: present_slot(),
            evidence: vec![
                range("cart.ts", "ignored", b"cart source text"),
                range("validate.ts", "ignored", b"validate target text"),
            ],
            candidates: vec![candidate("Symbol", "Symbol: validate")],
        },
    )
    .unwrap();
    assert_eq!(result.method, ReconcileMethod::LegacyTextV1);
    assert_eq!(result.proposal_schema_version, 1);
    assert_eq!(result.outcome, ReconciliationOutcome::Reconciled);
    for item in &result.evidence {
        assert_eq!(item.original, OriginalBinding::WorkingTreeUnverified);
        assert_eq!(item.outcome, ItemOutcome::RecaptureCorroborated);
        assert!(item.reconciled.is_some());
    }
    assert_eq!(result.candidates[0].outcome, CandidateOutcome::Matched);
    result.validate_for(&record).unwrap();
}

#[test]
fn a_v2_record_reports_retained_and_corroborated_items_apart() {
    let record = v2_record();
    let task = fixture();
    let text = |i: usize| task.task().evidence[i].text.as_bytes().to_vec();
    let (source, gap, target) = (text(0), text(1), text(2));
    let d = digest();
    let result = ReconciliationResultV1::derive(
        &record,
        ReconciliationInput {
            recovery: recovery(),
            slot: present_slot(),
            evidence: vec![
                range("source.ts", &d, &source),
                range("gap.ts", &d, &gap),
                range("target.ts", &d, &target),
            ],
            candidates: vec![candidate("Symbol", "unpublished candidate summary 87613")],
        },
    )
    .unwrap();
    assert_eq!(result.method, ReconcileMethod::CapturedSpanV2);
    assert_eq!(result.outcome, ReconciliationOutcome::Reconciled);
    let outcomes: Vec<_> = result.evidence.iter().map(|item| item.outcome).collect();
    assert_eq!(
        outcomes,
        [
            ItemOutcome::RetainedMatch,
            ItemOutcome::RecaptureCorroborated,
            ItemOutcome::RecaptureCorroborated
        ]
    );
    assert!(matches!(
        result.evidence[0].original,
        OriginalBinding::Captured { .. }
    ));
    result.validate_for(&record).unwrap();
}

#[test]
fn each_item_outcome_has_its_own_cause() {
    let record = v2_record();
    let staged = &record.basis.evidence[0];
    let d = digest();
    let text = staged.text_bytes;
    let good = vec![b'x'; text];
    let cases: Vec<(RecapturedItem<'_>, ItemOutcome)> = vec![
        (
            RecapturedItem::FactAbsent,
            ItemOutcome::Missing {
                reason: MissingReason::FactAbsent,
            },
        ),
        (
            RecapturedItem::PathNotCaptured,
            ItemOutcome::Missing {
                reason: MissingReason::PathNotCaptured,
            },
        ),
        (
            RecapturedItem::SpanOutOfBounds,
            ItemOutcome::Missing {
                reason: MissingReason::SpanOutOfBounds,
            },
        ),
        (
            RecapturedItem::NoParticipatingReceipt,
            ItemOutcome::Unverifiable {
                reason: UnverifiableReason::NoParticipatingReceipt,
            },
        ),
        (
            RecapturedItem::ValidationByteLimit,
            ItemOutcome::Unverifiable {
                reason: UnverifiableReason::ValidationByteLimit,
            },
        ),
        (
            range("source.ts", "node-v1:other", &good),
            ItemOutcome::Changed {
                reason: ChangedReason::FactDigestDiffers,
            },
        ),
        (
            range("source.ts", &d, &good),
            ItemOutcome::Changed {
                reason: ChangedReason::SpanBytesDiffer,
            },
        ),
    ];
    for (current, expected) in cases {
        let (outcome, reconciled) = compare_captured(staged, &d, current);
        assert_eq!(outcome, expected);
        assert!(reconciled.is_none());
    }
    // A range whose bytes do not fill its span is not an exact cover.
    let short = RecapturedItem::Range {
        origin: origin("source.ts", text + 1),
        fact_digest: &d,
        bytes: &good,
    };
    assert_eq!(
        compare_captured(staged, &d, short).0,
        ItemOutcome::Missing {
            reason: MissingReason::SpanOutOfBounds
        }
    );
    // A working-tree item whose recaptured span is not strict UTF-8.
    assert_eq!(
        compare_working_tree(staged, range("source.ts", &d, b"\xff\xfe")).0,
        ItemOutcome::Unverifiable {
            reason: UnverifiableReason::LossyText
        }
    );
    assert_eq!(
        compare_working_tree(staged, range("source.ts", &d, b"other text")).0,
        ItemOutcome::Changed {
            reason: ChangedReason::SpanBytesDiffer
        }
    );
}

#[test]
fn candidate_and_slot_outcomes() {
    let record = v1_record();
    let staged = &record.basis.candidates[0];
    assert_eq!(
        compare_candidate(staged, &RecapturedCandidate::Absent),
        CandidateOutcome::Missing
    );
    assert_eq!(
        compare_candidate(staged, &candidate("Function", "Symbol: validate")),
        CandidateOutcome::Changed {
            reason: CandidateChangedReason::LabelDiffers
        }
    );
    assert_eq!(
        compare_candidate(staged, &candidate("Symbol", "Symbol: validated")),
        CandidateOutcome::Changed {
            reason: CandidateChangedReason::SummaryDiffers
        }
    );
    let slot = |source, gap, relation| {
        SlotReconciliation {
            source,
            gap,
            relation,
        }
        .record()
    };
    use GapPresence as G;
    use SlotPresence as P;
    assert_eq!(
        slot(P::Present, G::Present, P::Present),
        ReconciliationOutcome::Reconciled
    );
    assert_eq!(
        slot(P::Missing, G::Present, P::Present),
        ReconciliationOutcome::Missing
    );
    assert_eq!(
        slot(P::Present, G::Present, P::Missing),
        ReconciliationOutcome::Missing
    );
    assert_eq!(
        slot(P::Missing, G::GapResolved, P::Present),
        ReconciliationOutcome::Changed
    );
}

fn derive_v1(
    evidence: Vec<RecapturedItem<'_>>,
    candidates: Vec<RecapturedCandidate>,
) -> ReconciliationResultV1 {
    ReconciliationResultV1::derive(
        &v1_record(),
        ReconciliationInput {
            recovery: recovery(),
            slot: present_slot(),
            evidence,
            candidates,
        },
    )
    .unwrap()
}

#[test]
fn outcome_precedence_is_changed_missing_unverifiable_reconciled() {
    let ok = || range("cart.ts", "", b"cart source text");
    let matched = || vec![candidate("Symbol", "Symbol: validate")];
    let unverifiable = derive_v1(
        vec![ok(), RecapturedItem::NoParticipatingReceipt],
        matched(),
    );
    assert_eq!(unverifiable.outcome, ReconciliationOutcome::Unverifiable);
    let missing = derive_v1(
        vec![
            RecapturedItem::FactAbsent,
            RecapturedItem::NoParticipatingReceipt,
        ],
        matched(),
    );
    assert_eq!(missing.outcome, ReconciliationOutcome::Missing);
    let changed = derive_v1(
        vec![
            RecapturedItem::FactAbsent,
            RecapturedItem::NoParticipatingReceipt,
        ],
        vec![candidate("Symbol", "Symbol: other")],
    );
    assert_eq!(changed.outcome, ReconciliationOutcome::Changed);
    // Every item outcome is still reported under a worse record outcome.
    assert_eq!(changed.evidence.len(), 2);
    assert_eq!(
        changed.evidence[0].outcome,
        ItemOutcome::Missing {
            reason: MissingReason::FactAbsent
        }
    );
}

#[test]
fn result_identity_is_canonical_and_the_decoder_is_strict() {
    let result = derive_v1(
        vec![
            range("cart.ts", "", b"cart source text"),
            range("validate.ts", "", b"validate target text"),
        ],
        vec![candidate("Symbol", "Symbol: validate")],
    );
    assert!(result.result_id.starts_with(RESULT_ID_PREFIX));
    let bytes = result.canonical_bytes().unwrap();
    assert_eq!(ReconciliationResultV1::decode(&bytes).unwrap(), result);
    // Key order on the wire does not change the identity.
    let pretty = serde_json::to_vec_pretty(&result).unwrap();
    assert_eq!(
        ReconciliationResultV1::decode(&pretty).unwrap().result_id,
        result.result_id
    );
    // Deriving again from the same comparisons gives the same id.
    let again = derive_v1(
        vec![
            range("cart.ts", "", b"cart source text"),
            range("validate.ts", "", b"validate target text"),
        ],
        vec![candidate("Symbol", "Symbol: validate")],
    );
    assert_eq!(again.result_id, result.result_id);
    // A different recovery is a different result.
    let mut moved = result.clone();
    moved.recovery.job_id = 8;
    assert_ne!(moved.compute_id().unwrap(), result.result_id);

    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let reject = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut tampered = value.clone();
        edit(&mut tampered);
        ReconciliationResultV1::decode(&serde_json::to_vec(&tampered).unwrap()).unwrap_err();
    };
    reject(&|v| v["result_id"] = json!(format!("{RESULT_ID_PREFIX}{}", "0".repeat(64))));
    reject(&|v| v["outcome"] = json!("changed"));
    reject(&|v| v["schema_version"] = json!(2));
    reject(&|v| v["extra"] = json!(true));
    reject(&|v| v["evidence"][0]["extra"] = json!(true));
    reject(&|v| v["method"] = json!("captured_span_v2"));
    reject(&|v| v["recovery"]["captures"] = json!([]));
    reject(&|v| v["evidence"][0]["reconciled"] = json!(null));
    reject(&|v| v["evidence"][0]["outcome"] = json!({"kind": "retained_match"}));
    reject(&|v| v["evidence"][0]["outcome"] = json!({"kind": "missing", "reason": "nope"}));
}

#[test]
fn a_result_binds_only_its_own_record() {
    let v1 = v1_record();
    let result = derive_v1(
        vec![RecapturedItem::FactAbsent, RecapturedItem::FactAbsent],
        vec![RecapturedCandidate::Absent],
    );
    result.validate_for(&v1).unwrap();
    result.validate_for(&v2_record()).unwrap_err();
    let mut swapped = result.clone();
    swapped.evidence.swap(0, 1);
    swapped.result_id = swapped.compute_id().unwrap();
    swapped.validate_for(&v1).unwrap_err();
    // Too few comparisons for the staged items is refused, not padded.
    ReconciliationResultV1::derive(
        &v1,
        ReconciliationInput {
            recovery: recovery(),
            slot: present_slot(),
            evidence: vec![RecapturedItem::FactAbsent],
            candidates: vec![RecapturedCandidate::Absent],
        },
    )
    .unwrap_err();
}
