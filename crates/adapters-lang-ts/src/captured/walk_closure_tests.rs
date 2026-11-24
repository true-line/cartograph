//! Completion-decided receipts of a captured directory walk are established
//! on the walk's closure when completion's configs and lookups are bound to
//! the capture (ADR-0072 slice 6, AC-1094).

use super::*;
use source_capture::{CaptureLimits, SourceId as CaptureSourceId, capture_working_tree};
use std::fs;

const SOURCE: &str = "src_11111111111111111111111111111111";
const REPO: &str = "local/src_11111111111111111111111111111111";
const RULE: &str =
    "export function ready(ok: boolean) { if (!ok) return missing(); return true; }\n";

fn write(root: &Path, path: &str, body: &str) {
    let file = root.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, body).unwrap();
}

/// Capture the host's selection (sources and configs) and walk it.
fn walk(root: &Path) -> Vec<MintedReceipt> {
    let selection = enumerate_selection(root).unwrap();
    let mut paths = selection.files.clone();
    paths.extend(selection.configs.iter().cloned());
    paths.sort();
    paths.dedup();
    let capture = capture_working_tree(
        root,
        &CaptureSourceId::new(SOURCE).unwrap(),
        &paths,
        CaptureLimits::default(),
    )
    .unwrap();
    extract_captured_dir(
        root,
        &SourceId {
            repo: REPO,
            commit: "workdir",
        },
        &capture,
        Registries::builtin(),
        &mut |_| {},
    )
    .unwrap()
    .1
}

/// The completion-decided receipt of `prefix` (`file:`, `test:` or
/// `gap:call:`) for `path`.
fn decided<'a>(
    receipts: &'a [MintedReceipt],
    prefix: &str,
    path: &str,
) -> Option<&'a MintedReceipt> {
    receipts.iter().find(|receipt| {
        receipt.file().path == path
            && matches!(receipt.fact_key(), FactKey::Node { id } if id.starts_with(prefix))
    })
}

fn walk_record(receipt: &MintedReceipt) -> &InputClosureV1 {
    let record = receipt
        .input_closure()
        .expect("an established walk closure");
    assert_eq!(
        receipt.closure(),
        Some(&ReceiptClosure::Established {
            closure_id: record.closure_id.clone()
        })
    );
    record
}

// AC-1094: with completion's configs and lookups bound, a File and an
// unresolved-call Gap receipt are established on their file's walk closure:
// the capture's enumeration under `source-walk-v2`, the file, each config
// and each path looked up and absent. A file's decided receipts share it.
#[test]
fn completion_decided_receipts_are_established_on_the_walk_closure() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(
        root,
        "src/rule.ts",
        &format!("import './style.css';\nimport {{ missing }} from './lib';\n{RULE}"),
    );
    write(root, "package.json", r#"{"name":"app"}"#);
    let receipts = walk(root);
    let file = decided(&receipts, "file:", "src/rule.ts").unwrap();
    let gap = decided(&receipts, "gap:call:", "src/rule.ts").unwrap();
    let record = walk_record(file);
    assert_eq!(walk_record(gap), record);
    assert!(file.id().starts_with("ts-primary-v4:"));
    assert_eq!(record.parameters.walk_policy, TS_DIRECTORY_WALK_POLICY);
    assert_eq!(record.parameters.repo_key, REPO);
    let capture_id = file.file().capture_id.clone();
    let kinds: Vec<String> = record
        .inputs
        .iter()
        .map(|input| match input {
            ClosureInput::CapturedFile { file } => format!("file {}", file.path),
            ClosureInput::Enumeration {
                capture_id: id,
                walk_policy,
                ..
            } => {
                assert_eq!(id, &capture_id);
                format!("enumeration {walk_policy}")
            }
            ClosureInput::AbsentFromEnumeration { path, .. } => format!("absent {path}"),
            other => panic!("unexpected input {other:?}"),
        })
        .collect();
    let mut kinds = kinds;
    kinds.sort();
    assert_eq!(
        kinds,
        [
            "absent src/style.css",
            "enumeration source-walk-v2",
            "file package.json",
            "file src/rule.ts"
        ]
    );
}

// AC-1094: adding the target of a previously unresolved call removes the
// Gap and changes the file's walk closure; adding an unrelated file changes
// the enumeration and the closure id but not the File fact.
#[test]
fn the_walk_closure_follows_the_enumeration() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(
        root,
        "rule.ts",
        &format!("import {{ missing }} from './lib';\n{RULE}"),
    );
    let before = walk(root);
    let file_before = decided(&before, "file:", "rule.ts").unwrap();
    write(root, "unrelated.ts", "export const unrelated = 1;\n");
    let after = walk(root);
    let file_after = decided(&after, "file:", "rule.ts").unwrap();
    assert_eq!(file_before.fact_key(), file_after.fact_key());
    assert_eq!(file_before.fact_digest(), file_after.fact_digest());
    assert_ne!(
        walk_record(file_before).closure_id,
        walk_record(file_after).closure_id
    );
    assert!(decided(&after, "gap:call:", "rule.ts").is_some());
    write(
        root,
        "lib.ts",
        "export function missing() { return false; }\n",
    );
    let resolved = walk(root);
    assert!(decided(&resolved, "gap:call:", "rule.ts").is_none());
    let file_resolved = decided(&resolved, "file:", "rule.ts").unwrap();
    assert_ne!(
        walk_record(file_after).closure_id,
        walk_record(file_resolved).closure_id
    );
}

// AC-1094: a tsconfig is bound by its retained bytes; with a config the
// capture does not hold, the decided receipts are `cross_file_input_unbound`.
#[test]
fn a_config_read_unbound_leaves_decided_receipts_unestablished() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "rule.ts", RULE);
    write(root, "tsconfig.json", "{}");
    let receipts = walk(root);
    let record = walk_record(decided(&receipts, "file:", "rule.ts").unwrap());
    assert!(record.inputs.iter().any(
        |input| matches!(input, ClosureInput::CapturedFile { file } if file.path == "tsconfig.json")
    ));
    write(root, "lib/package.json", r#"{"name":"lib"}"#);
    let selection = enumerate_selection(root).unwrap();
    let stale: Vec<String> = selection
        .files
        .iter()
        .chain(
            selection
                .configs
                .iter()
                .filter(|path| *path != "lib/package.json"),
        )
        .cloned()
        .collect();
    let capture = capture_working_tree(
        root,
        &CaptureSourceId::new(SOURCE).unwrap(),
        &stale,
        CaptureLimits::default(),
    )
    .unwrap();
    let (_, receipts, _) = extract_captured_dir(
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
    let file = decided(&receipts, "file:", "rule.ts").unwrap();
    assert_eq!(
        file.closure(),
        Some(&ReceiptClosure::InputClosureNotEstablished {
            reasons: vec![ClosureGapReason::CrossFileInputUnbound]
        })
    );
    assert!(file.input_closure().is_none());
}

/// The receipt of the edge out of `path` whose destination starts with
/// `prefix`.
fn decided_edge<'a>(
    receipts: &'a [MintedReceipt],
    path: &str,
    prefix: &str,
) -> Option<&'a MintedReceipt> {
    receipts.iter().find(|receipt| {
        receipt.file().path == path
            && matches!(receipt.fact_key(), FactKey::Edge { destination, .. }
                if destination.starts_with(prefix))
    })
}

// AC-1096: a cross-file call the walk resolves as parsed has a CALLS
// receipt, and an unresolved one a receipt for its Gap edge, both
// established on the caller's walk closure; resolving the call replaces
// the Gap edge's receipt with the CALLS edge's.
#[test]
fn walk_decided_call_edges_are_established_on_the_walk_closure() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(
        root,
        "rule.ts",
        &format!("import {{ missing }} from './lib';\n{RULE}"),
    );
    let receipts = walk(root);
    let gap = decided_edge(&receipts, "rule.ts", "gap:call:").expect("a Gap edge receipt");
    assert_eq!(
        walk_record(gap),
        walk_record(decided(&receipts, "file:", "rule.ts").unwrap())
    );
    write(
        root,
        "lib.ts",
        "export function missing() { return false; }\n",
    );
    let resolved = walk(root);
    assert!(decided_edge(&resolved, "rule.ts", "gap:call:").is_none());
    let target = format!("sym:{REPO}@lib.ts#missing");
    let call = decided_edge(&resolved, "rule.ts", &target).expect("a CALLS receipt");
    assert!(
        matches!(call.fact_key(), FactKey::Edge { label, destination, .. }
        if label == "CALLS" && *destination == target)
    );
    assert_eq!(
        walk_record(call),
        walk_record(decided(&resolved, "file:", "rule.ts").unwrap())
    );
    // Two calls to one target are one CALLS fact: one receipt, the one
    // whose complete edge completion kept.
    write(
        root,
        "twice.ts",
        "import { missing } from './lib';\nexport function twice() { missing(); return missing(); }\n",
    );
    let twice = walk(root);
    let calls = twice
        .iter()
        .filter(|receipt| {
            receipt.file().path == "twice.ts"
                && matches!(receipt.fact_key(), FactKey::Edge { destination, .. }
                    if *destination == target)
        })
        .count();
    assert_eq!(calls, 1);
}

// AC-1098: a fact directory completion emits itself is receipted against
// the file its evidence cites and established on that file's walk closure,
// here a Pulumi reference to a resource another module exports. AC-1520: a
// call resolved through a barrel also cites the barrel, which its receipt
// binds as another file; a fact citing a config keeps none.
#[test]
fn completion_emitted_facts_are_established_on_the_walk_closure() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "Pulumi.yaml", "name: app\nruntime: nodejs\n");
    write(
        root,
        "package.json",
        r#"{"dependencies":{"@pulumi/aws":"1"}}"#,
    );
    write(
        root,
        "index.ts",
        "import * as aws from '@pulumi/aws';\nimport { bucket } from './bucket';\n\
         new aws.lambda.Function('f', { environment: { variables: { B: bucket.arn } } });\n",
    );
    write(
        root,
        "bucket.ts",
        "import * as aws from '@pulumi/aws';\nexport const bucket = new aws.s3.Bucket('b');\n",
    );
    write(
        root,
        "rule.ts",
        &format!("import {{ missing }} from './barrel';\n{RULE}"),
    );
    write(root, "barrel.ts", "export { missing } from './lib';\n");
    write(
        root,
        "lib.ts",
        "export function missing() { return false; }\n",
    );
    let receipts = walk(root);
    let reference = receipts
        .iter()
        .find(|receipt| {
            matches!(receipt.fact_key(), FactKey::Edge { label, destination, .. }
                if label == "REFERENCES" && destination.contains("s3/bucket:Bucket:b"))
        })
        .expect("a Pulumi REFERENCES receipt");
    assert_eq!(reference.file().path, "index.ts");
    assert_eq!(
        walk_record(reference),
        walk_record(decided(&receipts, "file:", "index.ts").unwrap())
    );
    let target = format!("sym:{REPO}@lib.ts#missing");
    let call = decided_edge(&receipts, "rule.ts", &target).expect("a re-export CALLS receipt");
    assert_eq!(
        call.other_files()
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["barrel.ts"]
    );
    assert_eq!(
        walk_record(call),
        walk_record(decided(&receipts, "file:", "rule.ts").unwrap())
    );
    // A Next API route also cites the `package.json` that declares Next, a
    // config no receipt binds, so it keeps none.
    write(
        root,
        "pages/api/hello.ts",
        "export default function handler(req, res) { res.status(200).json({}); }\n",
    );
    write(
        root,
        "package.json",
        r#"{"dependencies":{"@pulumi/aws":"1","next":"14"}}"#,
    );
    let receipts = walk(root);
    assert!(!receipts.iter().any(|receipt| {
        matches!(receipt.fact_key(), FactKey::Node { id } if id.ends_with("@ALL:/api/hello"))
    }));
    assert!(decided_edge(&receipts, "rule.ts", &target).is_some());
}

// AC-1520: a TypeScript receipt binds other files only when every range is
// provenance; the same multi-file receipt, re-signed with a rule's range
// role, is refused.
#[test]
fn a_multi_file_typescript_receipt_is_provenance_only() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(
        root,
        "rule.ts",
        &format!("import {{ missing }} from './barrel';\n{RULE}"),
    );
    write(root, "barrel.ts", "export { missing } from './lib';\n");
    write(
        root,
        "lib.ts",
        "export function missing() { return false; }\n",
    );
    let receipts = walk(root);
    let target = format!("sym:{REPO}@lib.ts#missing");
    let receipt = decided_edge(&receipts, "rule.ts", &target)
        .unwrap()
        .to_receipt()
        .unwrap();
    assert!(Receipt::from_json(&receipt.to_json().unwrap()).is_ok());
    assert!(
        receipt
            .ranges()
            .iter()
            .all(|range| range.role == RangeRole::Provenance)
    );
    // The barrel's range as a rule exit is a well-formed rule inventory
    // (provenance, then the exit), refused only because it binds two files.
    let rule = receipt::resigned_with_role(receipt, RangeRole::RuleExit);
    assert!(matches!(
        rule.to_json(),
        Err(CapturedError::Invalid("receipt files"))
    ));
}
