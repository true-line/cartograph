//! Schedule reader receipts are producer-keyed and established on the
//! reader's closure over the captured files it read, when the reader
//! reports its facts bound to them (ADR-0072 slice 9, AC-1532).

use super::*;
use source_capture::{CaptureLimits, SourceId as CaptureSourceId, capture_working_tree};

const SOURCE: &str = "src_11111111111111111111111111111111";
const REPO: &str = "local/src_11111111111111111111111111111111";
const YACRON: &str = "cron.yaml";
const SCRIPT: &str = "jobs/cleanup.py";

/// Capture a yacron file, the script it runs and a TS file, read the
/// schedules from the capture, and stage their receipts.
fn walk(root: &Path, at: &str) -> (Vec<MintedReceipt>, Vec<String>) {
    std::fs::create_dir_all(root.join("jobs")).unwrap();
    std::fs::write(
        root.join(YACRON),
        format!("jobs:\n  - name: cleanup\n    command: python {SCRIPT}\n    schedule: \"{at}\"\n"),
    )
    .unwrap();
    std::fs::write(root.join(SCRIPT), "print('clean')\n").unwrap();
    std::fs::write(root.join("app.ts"), "export const a = 1;\n").unwrap();
    let paths: Vec<String> = [YACRON, SCRIPT, "app.ts"].map(String::from).to_vec();
    let capture = capture_working_tree(
        root,
        &CaptureSourceId::new(SOURCE).unwrap(),
        &paths,
        CaptureLimits::default(),
    )
    .unwrap();
    let read = |path: &str| {
        capture
            .file(path)
            .map(|file| file.bytes().to_vec())
            .map_err(|_| std::io::Error::other("not captured"))
    };
    let (facts, reads) = ingest::schedules::extract_from_capture_reading(
        root,
        &paths,
        &read,
        &ingest::schedules::SourceId {
            repo: REPO,
            commit: "workdir",
        },
        &std::collections::BTreeSet::new(),
    )
    .unwrap();
    assert!(reads.bound, "{reads:?}");
    assert!(!facts.nodes.is_empty());
    let id = SourceId {
        repo: REPO,
        commit: "workdir",
    };
    let unbound = stage_schedule_receipts(
        &capture,
        &reads.read,
        &id,
        (&facts.nodes, &facts.edges),
        false,
    )
    .into_receipts();
    assert!(!unbound.is_empty());
    for receipt in &unbound {
        assert_eq!(
            receipt.closure(),
            Some(&ReceiptClosure::InputClosureNotEstablished {
                reasons: vec![ClosureGapReason::CrossFileInputUnbound]
            })
        );
    }
    let receipts = stage_schedule_receipts(
        &capture,
        &reads.read,
        &id,
        (&facts.nodes, &facts.edges),
        true,
    )
    .into_receipts();
    (receipts, reads.read)
}

// AC-1532: the schedule reader's facts over a capture, reported bound, get
// contract-v4 Schedule receipts whatever lane the cited file's path
// selects, established on one closure: the capture's enumeration under
// `schedule-walk-v1` and each captured file the reader read. A change to
// the schedule changes the closure.
#[test]
fn schedule_receipts_are_established_on_the_reader_closure() {
    let directory = tempfile::tempdir().unwrap();
    let (before, read) = walk(directory.path(), "0 0 * * *");
    assert!(read.contains(&YACRON.to_string()), "{read:?}");
    let schedule: Vec<&MintedReceipt> = before
        .iter()
        .filter(|receipt| receipt.file().path == YACRON)
        .collect();
    assert!(!schedule.is_empty());
    assert_ne!(Grammar::for_path(YACRON), Grammar::Schedule);
    let record = schedule[0].input_closure().expect("an established closure");
    for receipt in &before {
        assert!(
            receipt.id().starts_with("sched-primary-v2:"),
            "{}",
            receipt.id()
        );
        assert_eq!(receipt.input_closure(), Some(record));
        let full = receipt.to_receipt().unwrap();
        assert_eq!(full.grammar(), Grammar::Schedule);
        assert_eq!(Receipt::from_json(&full.to_json().unwrap()).unwrap(), full);
    }
    assert_eq!(record.parameters.walk_policy, SCHEDULE_WALK_POLICY);
    assert_eq!(record.producer_contract, "t0.schedules/declared-v2");
    let mut inputs: Vec<String> = record
        .inputs
        .iter()
        .map(|input| match input {
            ClosureInput::CapturedFile { file } => format!("file {}", file.path),
            ClosureInput::Enumeration { walk_policy, .. } => format!("enumeration {walk_policy}"),
            other => panic!("unexpected input {other:?}"),
        })
        .collect();
    inputs.sort();
    // A probed location the capture does not hold (the root `Dockerfile`)
    // is the enumeration's answer, not a file input.
    assert!(read.contains(&"Dockerfile".to_string()), "{read:?}");
    let expected = [
        "enumeration schedule-walk-v1".to_string(),
        format!("file {YACRON}"),
        format!("file {SCRIPT}"),
    ];
    assert_eq!(inputs, expected);
    let (after, _) = walk(directory.path(), "0 1 * * *");
    let changed = after
        .iter()
        .find(|receipt| receipt.file().path == YACRON)
        .expect("the schedule is still receipted");
    assert_ne!(
        changed.input_closure().unwrap().closure_id,
        record.closure_id
    );
}

// AC-1532: the schedule grammar is chosen by the producer, never by a path,
// and records a closure state.
#[test]
fn the_schedule_grammar_is_producer_keyed() {
    assert!(Grammar::Schedule.producer_keyed());
    assert!(!Grammar::Environment.producer_keyed());
    assert!(Grammar::Schedule.has_v4());
    for path in [YACRON, SCRIPT, "deploy/crontab", "Dockerfile"] {
        assert_ne!(Grammar::for_path(path), Grammar::Schedule);
    }
}
