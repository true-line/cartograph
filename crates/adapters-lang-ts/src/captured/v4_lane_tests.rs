//! The directory lane mints contract-v4 receipts with single-file input
//! closures (ADR-0072 slice 5, AC-1084).

use super::*;
use source_capture::{CaptureLimits, SourceId as CaptureSourceId, capture_working_tree};
use std::fs;

const SOURCE: &str = "src_11111111111111111111111111111111";
const REPO: &str = "local/src_11111111111111111111111111111111";

/// The closure records the receipts carry, each once, by id.
fn records(receipts: &[MintedReceipt]) -> Vec<InputClosureV1> {
    let records: BTreeMap<_, _> = receipts
        .iter()
        .filter_map(MintedReceipt::input_closure)
        .map(|record| (record.closure_id.clone(), record.clone()))
        .collect();
    records.into_values().collect()
}

fn walk(root: &Path) -> (Vec<MintedReceipt>, IncrementalStats) {
    let paths = enumerate_paths(root).unwrap();
    let capture = capture_working_tree(
        root,
        &CaptureSourceId::new(SOURCE).unwrap(),
        &paths,
        CaptureLimits::default(),
    )
    .unwrap();
    let (_, receipts, stats) = extract_captured_dir(
        root,
        &SourceId {
            repo: REPO,
            commit: "workdir",
        },
        &capture,
        Registries::builtin(),
        &mut |_| {},
    )
    .unwrap();
    (receipts, stats)
}

fn of<'a>(receipts: &'a [MintedReceipt], path: &str) -> Vec<&'a MintedReceipt> {
    receipts
        .iter()
        .filter(|receipt| receipt.file().path == path && !completed(receipt))
        .collect()
}

fn completed(receipt: &MintedReceipt) -> bool {
    matches!(receipt.fact_key(), FactKey::Node { id }
        if id.starts_with("file:") || id.starts_with("test:") || id.starts_with("gap:call:"))
}

// AC-1084: a rule or declaration receipt of a freshly parsed file is a v4
// receipt whose established closure binds exactly that file's retained bytes,
// the contract pins, the repository key and the fixed walk and grammar rules;
// every such receipt carries exactly the record it names.
#[test]
fn a_fresh_parse_establishes_each_fact_on_its_own_file() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("rule.ts"),
        b"export function ready(ok: boolean) { if (!ok) return false; return true; }\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("other.ts"),
        b"export function other() { return 1; }\n",
    )
    .unwrap();
    let (receipts, _) = walk(directory.path());
    let rule = of(&receipts, "rule.ts");
    assert!(!rule.is_empty());
    let records = records(&receipts);
    let closure = records
        .iter()
        .find(|closure| {
            closure.inputs
                == [ClosureInput::CapturedFile {
                    file: rule[0].file().clone(),
                }]
        })
        .expect("rule.ts closure");
    assert_eq!(closure.producer_contract, "t0.adapter-ts/direct-lexical-v4");
    assert_eq!(closure.parameters.repo_key, REPO);
    assert_eq!(closure.parameters.walk_policy, TS_WALK_POLICY);
    assert_eq!(closure.parameters.grammar_rule, TS_GRAMMAR_RULE);
    assert!(closure.parameters.layer_hints.is_empty());
    assert!(closure.gaps().is_empty());
    for receipt in &rule {
        assert!(
            receipt.id().starts_with("ts-primary-v4:"),
            "{}",
            receipt.id()
        );
        assert_eq!(
            receipt.closure(),
            Some(&ReceiptClosure::Established {
                closure_id: closure.closure_id.clone()
            })
        );
        let full = receipt.to_receipt().unwrap();
        assert_eq!(Receipt::from_json(&full.to_json().unwrap()).unwrap(), full);
    }
    // Each established receipt carries the record it names; no other does.
    for receipt in &receipts {
        let named = match receipt.closure() {
            Some(ReceiptClosure::Established { closure_id }) => Some(closure_id),
            _ => None,
        };
        assert_eq!(
            named,
            receipt.input_closure().map(|record| &record.closure_id)
        );
    }
    // One record per freshly parsed file.
    assert_eq!(records.len(), 2);
}

// AC-1084: changing another file of the walk leaves a fact's digest, its
// receipt's spans and its closure's inputs as they were, apart from the
// capture identity every file reference carries.
#[test]
fn another_file_of_the_walk_never_changes_a_direct_lexical_fact() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("rule.ts"),
        b"export function ready(ok: boolean) { if (!ok) return false; return true; }\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("other.ts"),
        b"export function other() { return 1; }\n",
    )
    .unwrap();
    let (before, _) = walk(directory.path());
    fs::write(
        directory.path().join("other.ts"),
        b"import { ready } from './rule';\nexport function other() { return ready(true) ? 2 : 3; }\n",
    )
    .unwrap();
    let (after, _) = walk(directory.path());
    let facts = |receipts: &[MintedReceipt]| {
        of(receipts, "rule.ts")
            .into_iter()
            .map(|receipt| {
                (
                    receipt.fact_key().clone(),
                    receipt.fact_digest().to_owned(),
                    receipt
                        .captured_spans()
                        .map(|span| (span.file.path, span.byte_start, span.byte_end))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(facts(&before), facts(&after));
    let file_inputs = |receipts: &[MintedReceipt]| {
        records(receipts)
            .into_iter()
            .filter_map(|closure| match closure.inputs.as_slice() {
                [ClosureInput::CapturedFile { file }] if file.path == "rule.ts" => Some((
                    file.digest.clone(),
                    file.byte_len,
                    closure.producer_packages.clone(),
                    closure.parameters.clone(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(file_inputs(&before), file_inputs(&after));
    assert_eq!(file_inputs(&before).len(), 1);
}

// AC-1084: a receipt whose fact directory completion decides depends on
// other files of the walk, so its closure is not established.
#[test]
fn a_completion_decided_receipt_is_cross_file_input_unbound() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("rule.ts"),
        b"export function ready(ok: boolean) { if (!ok) return missing(); return true; }\n",
    )
    .unwrap();
    let (receipts, _) = walk(directory.path());
    let decided: Vec<_> = receipts
        .iter()
        .filter(|receipt| completed(receipt))
        .collect();
    assert!(!decided.is_empty());
    for receipt in decided {
        assert_eq!(
            receipt.closure(),
            Some(&ReceiptClosure::InputClosureNotEstablished {
                reasons: vec![ClosureGapReason::CrossFileInputUnbound]
            })
        );
    }
}
