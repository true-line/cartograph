//! Provenance-only receipts for the captured Python (AC-1024), Go (AC-1026),
//! Java (AC-1031), Rust (AC-1033), Kotlin (AC-1035) and Terraform (AC-1053)
//! lanes (#385).
//!
//! Each adapter parses a captured `.py`, `.go`, `.tf`, `.java`, `.rs`, `.kt` or
//! `.kts` file's retained bytes; this module binds a lane's facts to the exact
//! captured spans their primary provenance cites. Rules and facts citing more
//! than one file get none. Facts are bound in their final form, after
//! directory resolution (AC-1037), so those the directory join builds or
//! rewrites are covered too; receipts are admitted after every TypeScript
//! receipt within the same extraction budget.

use super::{
    CapturedError, Charge, ClosureGapReason, Grammar, MAX_EXTRACTION_RECEIPT_BYTES,
    MAX_EXTRACTION_RECEIPTS, MintedReceipt, Receipt, ReceiptBudget, ReceiptClosure, SourceId,
    valid_path,
};
use core_graph::{Edge, Node};
use source_capture::CapturedFile;
use std::sync::Arc;

/// Receipts for the facts one captured Python file emitted. A fact whose
/// provenance leaves the file, or that carries a rule, gets none.
pub fn mint_python_file_receipts(
    file: &CapturedFile,
    id: &SourceId,
    nodes: &[Node],
    edges: &[Edge],
) -> Result<Vec<MintedReceipt>, CapturedError> {
    mint_lane_file_receipts(Grammar::Python, file, id, nodes, edges)
}

/// Receipts for the facts one captured Go file emitted (AC-1026), under the
/// same provenance-only rules as the Python lane.
pub fn mint_go_file_receipts(
    file: &CapturedFile,
    id: &SourceId,
    nodes: &[Node],
    edges: &[Edge],
) -> Result<Vec<MintedReceipt>, CapturedError> {
    mint_lane_file_receipts(Grammar::Go, file, id, nodes, edges)
}

/// Receipts for the facts one captured Java file emitted (AC-1031), under
/// the same provenance-only rules as the Python lane.
pub fn mint_java_file_receipts(
    file: &CapturedFile,
    id: &SourceId,
    nodes: &[Node],
    edges: &[Edge],
) -> Result<Vec<MintedReceipt>, CapturedError> {
    mint_lane_file_receipts(Grammar::Java, file, id, nodes, edges)
}

/// Receipts for the facts one captured Rust file emitted (AC-1033), under
/// the same provenance-only rules as the Python lane.
pub fn mint_rust_file_receipts(
    file: &CapturedFile,
    id: &SourceId,
    nodes: &[Node],
    edges: &[Edge],
) -> Result<Vec<MintedReceipt>, CapturedError> {
    mint_lane_file_receipts(Grammar::Rust, file, id, nodes, edges)
}

/// Receipts for the facts one captured Kotlin file emitted (AC-1035), under
/// the same provenance-only rules as the Python lane.
pub fn mint_kotlin_file_receipts(
    file: &CapturedFile,
    id: &SourceId,
    nodes: &[Node],
    edges: &[Edge],
) -> Result<Vec<MintedReceipt>, CapturedError> {
    mint_lane_file_receipts(Grammar::Kotlin, file, id, nodes, edges)
}

fn mint_lane_file_receipts(
    grammar: Grammar,
    file: &CapturedFile,
    id: &SourceId,
    nodes: &[Node],
    edges: &[Edge],
) -> Result<Vec<MintedReceipt>, CapturedError> {
    mint_lane_receipts_in(grammar, file, None, id, nodes, edges)
}

/// As [`mint_lane_file_receipts`]; with `capture`, a fact whose evidence also
/// cites other captured files of the same grammar is bound to each (AC-1039).
fn mint_lane_receipts_in(
    grammar: Grammar,
    file: &CapturedFile,
    capture: Option<&source_capture::Capture>,
    id: &SourceId,
    nodes: &[Node],
    edges: &[Edge],
) -> Result<Vec<MintedReceipt>, CapturedError> {
    let path = &file.reference().path;
    if (Grammar::for_path(path) != grammar && !grammar.producer_keyed()) || !valid_path(path) {
        return Err(CapturedError::Invalid("unsupported captured path"));
    }
    super::receipt::validate_lane_identity(grammar, file.reference(), id.repo)?;
    let mut header = None;
    let mut receipts = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let minted = nodes
        .iter()
        .map(|node| Receipt::for_node_as(grammar, file, capture, id, node))
        .chain(
            edges
                .iter()
                .map(|edge| Receipt::for_edge_as(grammar, file, capture, id, edge)),
        );
    for receipt in minted {
        let Ok(receipt) = receipt
            .and_then(|receipt| measured(receipt, file))
            .and_then(|receipt| MintedReceipt::compact(receipt, &mut header))
        else {
            continue;
        };
        // A fact emitted twice by one file is bound once.
        if seen.insert((receipt.fact_key().clone(), receipt.fact_digest().to_owned())) {
            receipts.push(receipt);
        }
    }
    Ok(receipts)
}

/// Attach capture-time source sizes (#1728, AC-1441), as the TypeScript lane
/// does, so a later read of this receipt's ranges reserves their measured
/// JSON size instead of the six-fold worst case. A receipt that also cites
/// other files carries no sizes: its ranges are not all in `file`.
fn measured(receipt: Receipt, file: &CapturedFile) -> Result<Receipt, CapturedError> {
    if receipt.other_files().is_empty() {
        receipt.with_source_sizes(file)
    } else {
        Ok(receipt)
    }
}

/// Receipt candidates for a lane's final facts, after directory resolution
/// (AC-1037). Each fact is minted against the captured file its first
/// primary evidence entry cites, file by file in `files` order, and staged
/// within the extraction budget. A fact whose evidence also cites other files
/// is bound to each of them when every one is captured under the same grammar
/// (AC-1039, at most [`super::MAX_RECEIPT_OTHER_FILES`] more); otherwise it
/// gets none.
pub fn stage_lane_receipts(
    grammar: Grammar,
    capture: &source_capture::Capture,
    files: &[String],
    id: &SourceId,
    nodes: &[Node],
    edges: &[Edge],
) -> ReceiptStage {
    stage_lane_receipts_with(grammar, capture, files, id, (nodes, edges), &|receipt| {
        Ok(receipt)
    })
}

/// How a Terraform walk's inputs are bound (ADR-0072 slice 9, AC-1526).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerraformWalk {
    /// The walk read only captured `.tf` files, and each local module
    /// directory it listed held the captured files alone.
    Bound,
    /// The walk read an input the capture does not bind, for this reason.
    NotEstablished(ClosureGapReason),
}

/// Terraform receipts for `files` (AC-1053) under contract v4 (AC-1526):
/// established on the walk's closure ([`super::terraform_walk_closure`])
/// when `walk` is bound, `limit_exceeded` when that closure would pass its
/// bound, else carrying the reason in `walk`. Staged as
/// [`stage_lane_receipts`] stages them; the shared closure record is
/// charged once.
pub fn stage_terraform_receipts(
    capture: &source_capture::Capture,
    files: &[String],
    id: &SourceId,
    (nodes, edges): (&[Node], &[Edge]),
    walk: TerraformWalk,
) -> ReceiptStage {
    let closure = match walk {
        TerraformWalk::Bound => super::terraform_walk_closure(capture, files, id.repo)
            .map_err(|_| ClosureGapReason::LimitExceeded),
        TerraformWalk::NotEstablished(reason) => Err(reason),
    };
    stage_walk_receipts(
        Grammar::Terraform,
        closure,
        capture,
        files,
        id,
        (nodes, edges),
    )
}

/// Pipeline reader receipts for `files` (AC-1528): minted from the facts the
/// reader emitted over the capture, which it reads alone, as contract-v4
/// receipts established on the reader's closure
/// ([`super::pipeline_walk_closure`]), or `limit_exceeded` when that
/// closure would pass its bound. Staged as [`stage_lane_receipts`] stages
/// them; the shared closure record is charged once.
pub fn stage_pipeline_receipts(
    capture: &source_capture::Capture,
    files: &[String],
    id: &SourceId,
    (nodes, edges): (&[Node], &[Edge]),
) -> ReceiptStage {
    let closure = super::pipeline_walk_closure(capture, files, id.repo)
        .map_err(|_| ClosureGapReason::LimitExceeded);
    stage_walk_receipts(
        Grammar::Pipeline,
        closure,
        capture,
        files,
        id,
        (nodes, edges),
    )
}

/// Environment reader receipts for `files` (AC-1530), staged as
/// [`stage_pipeline_receipts`] stages a pipeline's: established on the
/// reader's closure ([`super::environment_walk_closure`]) when `read` names
/// the files the reader opened over the capture and reported its facts
/// bound to, all of them among `files` (the closure binds each, whatever
/// its grammar), and `cross_file_input_unbound` otherwise.
pub fn stage_environment_receipts(
    capture: &source_capture::Capture,
    files: &[String],
    id: &SourceId,
    (nodes, edges): (&[Node], &[Edge]),
    read: Option<&[String]>,
) -> ReceiptStage {
    let closure = match read {
        Some(read) if read.iter().all(|path| files.contains(path)) => {
            super::environment_walk_closure(capture, files, read, id.repo)
                .map_err(|_| ClosureGapReason::LimitExceeded)
        }
        _ => Err(ClosureGapReason::CrossFileInputUnbound),
    };
    stage_walk_receipts(
        Grammar::Environment,
        closure,
        capture,
        files,
        id,
        (nodes, edges),
    )
}

/// Schedule reader receipts (AC-1532), staged as
/// [`stage_environment_receipts`] stages an environment's: each fact cites
/// a file among `read`, the captured files the reader opened, and is
/// established on the reader's closure ([`super::schedule_walk_closure`])
/// when the reader reported its facts `bound` to them, and
/// `cross_file_input_unbound` otherwise.
pub fn stage_schedule_receipts(
    capture: &source_capture::Capture,
    read: &[String],
    id: &SourceId,
    (nodes, edges): (&[Node], &[Edge]),
    bound: bool,
) -> ReceiptStage {
    let closure = if bound {
        super::schedule_walk_closure(capture, read, id.repo)
            .map_err(|_| ClosureGapReason::LimitExceeded)
    } else {
        Err(ClosureGapReason::CrossFileInputUnbound)
    };
    stage_walk_receipts(
        Grammar::Schedule,
        closure,
        capture,
        read,
        id,
        (nodes, edges),
    )
}

/// Entry-point reader receipts (AC-1534), staged as
/// [`stage_schedule_receipts`] stages a schedule's: `read` holds the
/// captured files the entry-point reader and the schedule reader read, and
/// `bound` is whether both reported their facts bound to them.
pub fn stage_entry_point_receipts(
    capture: &source_capture::Capture,
    read: &[String],
    id: &SourceId,
    (nodes, edges): (&[Node], &[Edge]),
    bound: bool,
) -> ReceiptStage {
    let closure = if bound {
        super::entry_point_walk_closure(capture, read, id.repo)
            .map_err(|_| ClosureGapReason::LimitExceeded)
    } else {
        Err(ClosureGapReason::CrossFileInputUnbound)
    };
    stage_walk_receipts(
        Grammar::EntryPoint,
        closure,
        capture,
        read,
        id,
        (nodes, edges),
    )
}

/// Toolchain reader receipts (AC-1536), staged as
/// [`stage_schedule_receipts`] stages a schedule's: `read` holds the
/// captured config files the toolchain reader read, and `bound` is whether
/// it reported its facts bound to them.
pub fn stage_toolchain_receipts(
    capture: &source_capture::Capture,
    read: &[String],
    id: &SourceId,
    (nodes, edges): (&[Node], &[Edge]),
    bound: bool,
) -> ReceiptStage {
    let closure = if bound {
        super::toolchain_walk_closure(capture, read, id.repo)
            .map_err(|_| ClosureGapReason::LimitExceeded)
    } else {
        Err(ClosureGapReason::CrossFileInputUnbound)
    };
    stage_walk_receipts(
        Grammar::Toolchain,
        closure,
        capture,
        read,
        id,
        (nodes, edges),
    )
}

/// Declared API reference receipts (AC-1538): `read` holds the captured
/// documents and config the loader read, which alone decide the facts
/// staged, so the closure is established unless it exceeds its bound.
pub fn stage_api_doc_receipts(
    capture: &source_capture::Capture,
    read: &[String],
    id: &SourceId,
    (nodes, edges): (&[Node], &[Edge]),
) -> ReceiptStage {
    let closure = super::api_docs_walk_closure(capture, read, id.repo)
        .map_err(|_| ClosureGapReason::LimitExceeded);
    stage_walk_receipts(Grammar::ApiDocs, closure, capture, read, id, (nodes, edges))
}

/// Stage `grammar`'s receipts for `files`, each established on `closure`
/// or carrying its reason.
fn stage_walk_receipts(
    grammar: Grammar,
    closure: Result<super::InputClosureV1, ClosureGapReason>,
    capture: &source_capture::Capture,
    files: &[String],
    id: &SourceId,
    (nodes, edges): (&[Node], &[Edge]),
) -> ReceiptStage {
    let (state, record) = match closure {
        Ok(record) => (
            ReceiptClosure::Established {
                closure_id: record.closure_id.clone(),
            },
            Some(Arc::new(record)),
        ),
        Err(reason) => (
            ReceiptClosure::InputClosureNotEstablished {
                reasons: vec![reason],
            },
            None,
        ),
    };
    stage_lane_receipts_with(grammar, capture, files, id, (nodes, edges), &|receipt| {
        receipt.with_closure(state.clone(), record.clone())
    })
}

fn stage_lane_receipts_with(
    grammar: Grammar,
    capture: &source_capture::Capture,
    files: &[String],
    id: &SourceId,
    (nodes, edges): (&[Node], &[Edge]),
    state: &dyn Fn(MintedReceipt) -> Result<MintedReceipt, CapturedError>,
) -> ReceiptStage {
    let mut by_file: std::collections::BTreeMap<&str, (Vec<&Node>, Vec<&Edge>)> =
        std::collections::BTreeMap::new();
    for node in nodes {
        if let Some(path) = first_evidence_path(&node.props) {
            by_file.entry(path).or_default().0.push(node);
        }
    }
    for edge in edges {
        if let Some(path) = first_evidence_path(&edge.props) {
            by_file.entry(path).or_default().1.push(edge);
        }
    }
    let mut stage = ReceiptStage::default();
    for path in files {
        let (Some((nodes, edges)), Ok(file)) = (by_file.get(path.as_str()), capture.file(path))
        else {
            continue;
        };
        let nodes: Vec<Node> = nodes.iter().map(|node| (*node).clone()).collect();
        let edges: Vec<Edge> = edges.iter().map(|edge| (*edge).clone()).collect();
        if let Ok(receipts) =
            mint_lane_receipts_in(grammar, file, Some(capture), id, &nodes, &edges).and_then(
                |receipts| {
                    receipts
                        .into_iter()
                        .map(state)
                        .collect::<Result<Vec<_>, _>>()
                },
            )
        {
            stage.extend(receipts);
        }
    }
    stage
}

/// The file a fact's first primary evidence entry cites: its receipt's
/// primary file.
pub(super) fn first_evidence_path(props: &serde_json::Value) -> Option<&str> {
    props
        .get("prov")?
        .get("evidence")?
        .get(0)?
        .get("path")?
        .as_str()
}

/// Python receipt candidates staged while the lane mints them, charged
/// against the extraction receipt budget as they arrive: once the staged
/// candidates reach it, later ones are dropped, so minting cannot outgrow the
/// budget before the survivor check and final admission run.
#[derive(Default)]
pub struct ReceiptStage {
    receipts: Vec<MintedReceipt>,
    charge: Charge,
}

impl ReceiptStage {
    /// Stage one file's candidates in order; those past the budget are dropped
    /// and their facts stay uncaptured.
    pub fn extend(&mut self, candidates: Vec<MintedReceipt>) {
        self.extend_within(candidates, EXTRACTION_BUDGET);
    }

    fn extend_within(&mut self, candidates: Vec<MintedReceipt>, budget: ReceiptBudget) {
        for receipt in candidates {
            if self.charge.admit(&receipt, self.receipts.len(), budget) {
                self.receipts.push(receipt);
            }
        }
    }

    /// The staged candidates, in staging order.
    pub fn into_receipts(self) -> Vec<MintedReceipt> {
        self.receipts
    }
}

const EXTRACTION_BUDGET: ReceiptBudget = ReceiptBudget {
    bytes: MAX_EXTRACTION_RECEIPT_BYTES,
    receipts: MAX_EXTRACTION_RECEIPTS,
};

/// Admit `candidates` after `held` within the extraction receipt budget, in
/// order; one that does not fit is left out and its fact stays uncaptured.
pub fn admit_after(held: &mut Vec<MintedReceipt>, candidates: Vec<MintedReceipt>) {
    admit_within(held, candidates, EXTRACTION_BUDGET)
}

fn admit_within(
    held: &mut Vec<MintedReceipt>,
    candidates: Vec<MintedReceipt>,
    budget: ReceiptBudget,
) {
    let unlimited = ReceiptBudget {
        bytes: usize::MAX,
        receipts: usize::MAX,
    };
    let mut charge = Charge::default();
    for (index, receipt) in held.iter().enumerate() {
        charge.admit(receipt, index, unlimited);
    }
    for receipt in candidates {
        if charge.admit(&receipt, held.len(), budget) {
            held.push(receipt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
    use serde_json::json;

    const SOURCE: &str = "def handler(user):\n    return user\n";

    fn prov(path: &str, span: (u64, u64)) -> serde_json::Value {
        serde_json::to_value(
            Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                vec![EvidenceRef {
                    repo: "local/src_11111111111111111111111111111111".into(),
                    path: path.into(),
                    byte_start: span.0,
                    byte_end: span.1,
                    commit_sha: "workdir".into(),
                }],
                "t0.adapter-python",
                b"fact",
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn node(id: &str, props: serde_json::Value) -> Node {
        Node {
            id: id.into(),
            label: "Symbol".into(),
            props,
        }
    }

    // AC-1024: a Python fact citing its own captured file gets a Python
    // receipt; a rule-bearing fact or one citing another file gets none.
    #[test]
    fn python_receipts_bind_only_provenance_in_the_captured_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.py"), SOURCE).unwrap();
        let capture = source_capture::capture_working_tree(
            dir.path(),
            &source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap(),
            &["app.py".to_string()],
            source_capture::CaptureLimits::default(),
        )
        .unwrap();
        let file = capture.file("app.py").unwrap();
        let id = SourceId {
            repo: "local/src_11111111111111111111111111111111",
            commit: "workdir",
        };
        let own = node(
            "sym:app.py#handler",
            json!({ "prov": prov("app.py", (4, 11)) }),
        );
        let elsewhere = node(
            "sym:other.py#x",
            json!({ "prov": prov("other.py", (0, 3)) }),
        );
        let rule = node(
            "rule:app.py#1",
            json!({ "prov": prov("app.py", (0, 3)), "rule": {} }),
        );
        let receipts =
            mint_python_file_receipts(file, &id, &[own.clone(), elsewhere, rule], &[]).unwrap();
        assert_eq!(receipts.len(), 1);
        let receipt = receipts[0].to_receipt().unwrap();
        assert!(receipt.id().starts_with("py-primary-v1:"));
        assert_eq!(receipt.grammar(), Grammar::Python);
        assert!(receipt.matches_node(&own));
        assert_eq!(
            Receipt::from_json(&receipt.to_json().unwrap()).unwrap(),
            receipt
        );
        assert_eq!(&SOURCE.as_bytes()[4..11], b"handler");
        // A TS file is no Python lane input.
        std::fs::write(dir.path().join("a.ts"), "export const a = 1;\n").unwrap();
        let ts = source_capture::capture_working_tree(
            dir.path(),
            &source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap(),
            &["a.ts".to_string()],
            source_capture::CaptureLimits::default(),
        )
        .unwrap();
        assert!(
            mint_python_file_receipts(
                ts.file("a.ts").unwrap(),
                &id,
                std::slice::from_ref(&own),
                &[]
            )
            .is_err()
        );
        // Nor is a Python file a Go lane input.
        assert!(mint_go_file_receipts(file, &id, &[own], &[]).is_err());
    }

    // AC-1026: a Go fact citing its own captured file gets a Go receipt under
    // its own prefix; a rule-bearing fact gets none.
    #[test]
    fn go_receipts_bind_only_provenance_in_the_captured_file() {
        const GO: &str = "package main\n\nfunc Handler() {}\n";
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), GO).unwrap();
        let capture = source_capture::capture_working_tree(
            dir.path(),
            &source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap(),
            &["main.go".to_string()],
            source_capture::CaptureLimits::default(),
        )
        .unwrap();
        let file = capture.file("main.go").unwrap();
        let id = SourceId {
            repo: "local/src_11111111111111111111111111111111",
            commit: "workdir",
        };
        let own = node(
            "sym:main.go#Handler",
            json!({ "prov": prov("main.go", (19, 26)) }),
        );
        let rule = node(
            "rule:main.go#1",
            json!({ "prov": prov("main.go", (0, 7)), "rule": {} }),
        );
        let receipts = mint_go_file_receipts(file, &id, &[own.clone(), rule], &[]).unwrap();
        assert_eq!(receipts.len(), 1);
        let receipt = receipts[0].to_receipt().unwrap();
        assert!(receipt.id().starts_with("go-primary-v1:"));
        assert_eq!(receipt.grammar(), Grammar::Go);
        assert!(receipt.matches_node(&own));
        assert_eq!(
            Receipt::from_json(&receipt.to_json().unwrap()).unwrap(),
            receipt
        );
        assert_eq!(&GO.as_bytes()[19..26], b"Handler");
        assert!(mint_python_file_receipts(file, &id, std::slice::from_ref(&own), &[]).is_err());
        assert!(mint_java_file_receipts(file, &id, &[own], &[]).is_err());
    }

    // AC-1031: a Java fact citing its own captured file gets a Java receipt
    // under its own prefix; a rule-bearing fact gets none.
    #[test]
    fn java_receipts_bind_only_provenance_in_the_captured_file() {
        const JAVA: &str = "class App {\n  void handle() {}\n}\n";
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("App.java"), JAVA).unwrap();
        let capture = source_capture::capture_working_tree(
            dir.path(),
            &source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap(),
            &["App.java".to_string()],
            source_capture::CaptureLimits::default(),
        )
        .unwrap();
        let file = capture.file("App.java").unwrap();
        let id = SourceId {
            repo: "local/src_11111111111111111111111111111111",
            commit: "workdir",
        };
        let own = node(
            "sym:App.java#handle",
            json!({ "prov": prov("App.java", (19, 25)) }),
        );
        let rule = node(
            "rule:App.java#1",
            json!({ "prov": prov("App.java", (0, 5)), "rule": {} }),
        );
        let receipts = mint_java_file_receipts(file, &id, &[own.clone(), rule], &[]).unwrap();
        assert_eq!(receipts.len(), 1);
        let receipt = receipts[0].to_receipt().unwrap();
        assert!(receipt.id().starts_with("java-primary-v1:"));
        assert_eq!(receipt.grammar(), Grammar::Java);
        assert!(receipt.matches_node(&own));
        assert_eq!(
            Receipt::from_json(&receipt.to_json().unwrap()).unwrap(),
            receipt
        );
        assert_eq!(&JAVA.as_bytes()[19..25], b"handle");
        assert!(mint_go_file_receipts(file, &id, std::slice::from_ref(&own), &[]).is_err());
        assert!(mint_rust_file_receipts(file, &id, &[own], &[]).is_err());
    }

    // AC-1441 (#1728): a lane receipt carries its ranges' measured JSON
    // sizes, so a read reserves those and not the six-fold worst case.
    #[test]
    fn lane_receipts_carry_measured_source_sizes() {
        const JAVA: &str = "class App {\n\tvoid check() { throw new Error(\"no\"); }\n}\n";
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("App.java"), JAVA).unwrap();
        let capture = source_capture::capture_working_tree(
            dir.path(),
            &source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap(),
            &["App.java".to_string()],
            source_capture::CaptureLimits::default(),
        )
        .unwrap();
        let file = capture.file("App.java").unwrap();
        let id = SourceId {
            repo: "local/src_11111111111111111111111111111111",
            commit: "workdir",
        };
        let span = (12, JAVA.len() as u64 - 3);
        let own = node(
            "sym:App.java#check",
            json!({ "prov": prov("App.java", span) }),
        );
        let receipts = mint_java_file_receipts(file, &id, &[own], &[]).unwrap();
        let receipt = receipts[0].to_receipt().unwrap();
        let text = &JAVA[span.0 as usize..span.1 as usize];
        let sizes = receipt.source_sizes(0).unwrap().unwrap();
        // The tab and the two quotes each escape to two bytes.
        assert_eq!(sizes.original, text.len() + 3);
        assert!(sizes.original < 6 * text.len());
        assert!(receipt.source_sizes_fit(0, text).unwrap());
        assert_eq!(
            Receipt::from_json(&receipt.to_json().unwrap()).unwrap(),
            receipt
        );
    }

    // AC-1033: a Rust fact citing its own captured file gets a Rust receipt
    // under its own prefix; a rule-bearing fact gets none.
    #[test]
    fn rust_receipts_bind_only_provenance_in_the_captured_file() {
        const RUST: &str = "pub fn handle() {}\n";
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), RUST).unwrap();
        let capture = source_capture::capture_working_tree(
            dir.path(),
            &source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap(),
            &["lib.rs".to_string()],
            source_capture::CaptureLimits::default(),
        )
        .unwrap();
        let file = capture.file("lib.rs").unwrap();
        let id = SourceId {
            repo: "local/src_11111111111111111111111111111111",
            commit: "workdir",
        };
        let own = node(
            "sym:lib.rs#handle",
            json!({ "prov": prov("lib.rs", (7, 13)) }),
        );
        let rule = node(
            "rule:lib.rs#1",
            json!({ "prov": prov("lib.rs", (0, 3)), "rule": {} }),
        );
        let receipts = mint_rust_file_receipts(file, &id, &[own.clone(), rule], &[]).unwrap();
        assert_eq!(receipts.len(), 1);
        let receipt = receipts[0].to_receipt().unwrap();
        assert!(receipt.id().starts_with("rs-primary-v1:"));
        assert_eq!(receipt.grammar(), Grammar::Rust);
        assert!(receipt.matches_node(&own));
        assert_eq!(
            Receipt::from_json(&receipt.to_json().unwrap()).unwrap(),
            receipt
        );
        assert_eq!(&RUST.as_bytes()[7..13], b"handle");
        assert!(mint_java_file_receipts(file, &id, std::slice::from_ref(&own), &[]).is_err());
        assert!(mint_kotlin_file_receipts(file, &id, &[own], &[]).is_err());
    }

    // AC-1035: a Kotlin fact citing its own captured `.kt` or `.kts` file gets
    // a Kotlin receipt under its own prefix; a rule-bearing fact gets none.
    #[test]
    fn kotlin_receipts_bind_only_provenance_in_the_captured_file() {
        const KOTLIN: &str = "fun handle() {}\n";
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("App.kt"), KOTLIN).unwrap();
        std::fs::write(dir.path().join("build.gradle.kts"), KOTLIN).unwrap();
        let capture = source_capture::capture_working_tree(
            dir.path(),
            &source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap(),
            &["App.kt".to_string(), "build.gradle.kts".to_string()],
            source_capture::CaptureLimits::default(),
        )
        .unwrap();
        let id = SourceId {
            repo: "local/src_11111111111111111111111111111111",
            commit: "workdir",
        };
        for path in ["App.kt", "build.gradle.kts"] {
            let file = capture.file(path).unwrap();
            let own = node(
                &format!("sym:{path}#handle"),
                json!({ "prov": prov(path, (4, 10)) }),
            );
            let rule = node(
                &format!("rule:{path}#1"),
                json!({ "prov": prov(path, (0, 3)), "rule": {} }),
            );
            let receipts = mint_kotlin_file_receipts(file, &id, &[own.clone(), rule], &[]).unwrap();
            assert_eq!(receipts.len(), 1);
            let receipt = receipts[0].to_receipt().unwrap();
            assert!(receipt.id().starts_with("kt-primary-v1:"));
            assert_eq!(receipt.grammar(), Grammar::Kotlin);
            assert!(receipt.matches_node(&own));
            assert_eq!(
                Receipt::from_json(&receipt.to_json().unwrap()).unwrap(),
                receipt
            );
            assert!(mint_rust_file_receipts(file, &id, &[own], &[]).is_err());
        }
        assert_eq!(&KOTLIN.as_bytes()[4..10], b"handle");
    }

    fn prov_spanning(spans: &[(&str, (u64, u64))]) -> serde_json::Value {
        let evidence = spans
            .iter()
            .map(|(path, span)| EvidenceRef {
                repo: "local/src_11111111111111111111111111111111".into(),
                path: (*path).into(),
                byte_start: span.0,
                byte_end: span.1,
                commit_sha: "workdir".into(),
            })
            .collect();
        serde_json::to_value(
            Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                evidence,
                "t0.adapter-python",
                b"fact",
            )
            .unwrap(),
        )
        .unwrap()
    }

    // AC-1037: final facts, including those the directory join builds from
    // several spans of one file, are bound per captured file in `files`
    // order; a fact citing a file outside the capture gets none.
    #[test]
    fn lane_receipts_bind_final_facts_citing_one_captured_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), SOURCE).unwrap();
        std::fs::write(dir.path().join("b.py"), SOURCE).unwrap();
        let capture = source_capture::capture_working_tree(
            dir.path(),
            &source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap(),
            &["a.py".to_string(), "b.py".to_string()],
            source_capture::CaptureLimits::default(),
        )
        .unwrap();
        let id = SourceId {
            repo: "local/src_11111111111111111111111111111111",
            commit: "workdir",
        };
        let joined = node(
            "ep:a",
            json!({ "prov": prov_spanning(&[("a.py", (0, 3)), ("a.py", (4, 11))]) }),
        );
        let nodes = [
            joined.clone(),
            node(
                "ep:outside",
                json!({ "prov": prov_spanning(&[("a.py", (0, 3)), ("c.py", (0, 3))]) }),
            ),
            node("sym:b", json!({ "prov": prov("b.py", (4, 11)) })),
            node("sym:c", json!({ "prov": prov("c.py", (0, 3)) })),
        ];
        let edges = [Edge {
            src: "ep:a".into(),
            dst: "sym:b".into(),
            label: "HANDLES".into(),
            props: json!({ "prov": prov("a.py", (0, 3)) }),
        }];
        let files = ["b.py".to_string(), "a.py".to_string(), "c.py".to_string()];
        let staged = stage_lane_receipts(Grammar::Python, &capture, &files, &id, &nodes, &edges)
            .into_receipts();
        let keys: Vec<_> = staged
            .iter()
            .map(|receipt| receipt.fact_key().clone())
            .collect();
        assert_eq!(
            keys,
            [
                core_graph::source::FactKey::Node { id: "sym:b".into() },
                core_graph::source::FactKey::Node { id: "ep:a".into() },
                core_graph::source::FactKey::Edge {
                    source: "ep:a".into(),
                    label: "HANDLES".into(),
                    destination: "sym:b".into(),
                },
            ]
        );
        let receipt = staged[1].to_receipt().unwrap();
        assert!(receipt.id().starts_with("py-primary-v1:"));
        assert!(receipt.matches_node(&joined));
        assert_eq!(receipt.ranges().len(), 2);
    }

    // AC-1039: a fact the join builds from spans of several captured files of
    // the lane's grammar gets one receipt whose primary file is the first one
    // cited and which lists the others; each range reads back from its own
    // file, the receipt round-trips, and its single-file shape is unchanged.
    #[test]
    fn lane_receipts_bind_facts_citing_several_captured_files() {
        let dir = tempfile::tempdir().unwrap();
        for path in ["routes.py", "app.py", "blog.py"] {
            std::fs::write(dir.path().join(path), SOURCE).unwrap();
        }
        std::fs::write(dir.path().join("view.ts"), "export const a = 1;\n").unwrap();
        let capture = source_capture::capture_working_tree(
            dir.path(),
            &source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap(),
            &[
                "app.py".to_string(),
                "blog.py".to_string(),
                "routes.py".to_string(),
                "view.ts".to_string(),
            ],
            source_capture::CaptureLimits::default(),
        )
        .unwrap();
        let id = SourceId {
            repo: "local/src_11111111111111111111111111111111",
            commit: "workdir",
        };
        // The route in routes.py, then its mounts in app.py and blog.py.
        let route = node(
            "ep:route",
            json!({ "prov": prov_spanning(&[
                ("routes.py", (4, 11)),
                ("blog.py", (0, 3)),
                ("app.py", (0, 3)),
                ("blog.py", (4, 11)),
            ]) }),
        );
        let mixed = node(
            "ep:mixed",
            json!({ "prov": prov_spanning(&[("routes.py", (0, 3)), ("view.ts", (0, 6))]) }),
        );
        let files = [
            "app.py".to_string(),
            "blog.py".to_string(),
            "routes.py".to_string(),
        ];
        let staged = stage_lane_receipts(
            Grammar::Python,
            &capture,
            &files,
            &id,
            &[route.clone(), mixed],
            &[],
        )
        .into_receipts();
        assert_eq!(staged.len(), 1, "a file of another grammar is never cited");
        let minted = &staged[0];
        let receipt = minted.to_receipt().unwrap();
        assert!(receipt.id().starts_with("py-primary-v1:"));
        assert_eq!(receipt.file().path, "routes.py");
        let others: Vec<_> = receipt
            .other_files()
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(others, ["app.py", "blog.py"]);
        // AC-1441: its ranges span files, so it carries no measured sizes.
        assert_eq!(receipt.source_sizes(0).unwrap(), None);
        assert!(receipt.matches_node(&route));
        assert!(minted.matches_node(&route));
        let cited: Vec<_> = receipt
            .ranges()
            .iter()
            .map(|range| range.captured.file.path.as_str())
            .collect();
        assert_eq!(cited, ["routes.py", "blog.py", "app.py", "blog.py"]);
        for range in receipt.ranges() {
            let file = capture.file(&range.captured.file.path).unwrap();
            assert_eq!(range.captured.file, *file.reference());
            assert!(
                file.span(range.captured.byte_start, range.captured.byte_end)
                    .is_ok()
            );
        }
        let spans: Vec<_> = minted.captured_spans().collect();
        assert_eq!(
            spans,
            receipt
                .ranges()
                .iter()
                .map(|range| range.captured.clone())
                .collect::<Vec<_>>()
        );
        let json = receipt.to_json().unwrap();
        assert_eq!(minted.to_json().unwrap(), json);
        assert_eq!(Receipt::from_json(&json).unwrap(), receipt);
        // Compact ranges name the other file by position; the primary by none.
        let wire: serde_json::Value = serde_json::from_str(&json).unwrap();
        let indices: Vec<_> = wire["content"]["ranges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|range| range.get("file").cloned())
            .collect();
        assert_eq!(
            indices,
            [None, Some(json!(1)), Some(json!(0)), Some(json!(1))]
        );
        // A single-file receipt carries no file list at all.
        let single = mint_python_file_receipts(
            capture.file("app.py").unwrap(),
            &id,
            &[node("sym:a", json!({ "prov": prov("app.py", (0, 3)) }))],
            &[],
        )
        .unwrap();
        assert!(!single[0].to_json().unwrap().contains("other_files"));
    }

    // AC-1039: every alternate or inconsistent spelling of the file list is
    // rejected, so a receipt has one wire form.
    #[test]
    fn receipts_citing_several_files_have_one_wire_form() {
        let dir = tempfile::tempdir().unwrap();
        for path in ["a.py", "b.py", "c.py"] {
            std::fs::write(dir.path().join(path), SOURCE).unwrap();
        }
        let capture = source_capture::capture_working_tree(
            dir.path(),
            &source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap(),
            &["a.py".to_string(), "b.py".to_string(), "c.py".to_string()],
            source_capture::CaptureLimits::default(),
        )
        .unwrap();
        let id = SourceId {
            repo: "local/src_11111111111111111111111111111111",
            commit: "workdir",
        };
        let fact = node(
            "ep:x",
            json!({ "prov": prov_spanning(&[("a.py", (0, 3)), ("b.py", (4, 11))]) }),
        );
        let staged = stage_lane_receipts(
            Grammar::Python,
            &capture,
            &["a.py".to_string()],
            &id,
            &[fact],
            &[],
        )
        .into_receipts();
        let json = staged[0].to_json().unwrap();
        let wire: serde_json::Value = serde_json::from_str(&json).unwrap();
        let tampered = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut value = wire.clone();
            edit(&mut value);
            Receipt::from_json(&value.to_string())
        };
        let c = serde_json::to_value(capture.file("c.py").unwrap().reference()).unwrap();
        // An empty list, a null index, an index past the list, an uncited
        // extra file, and a list naming the primary file are all refused.
        assert!(tampered(&|value| value["content"]["other_files"] = json!([])).is_err());
        assert!(tampered(&|value| value["content"]["ranges"][0]["file"] = json!(null)).is_err());
        assert!(tampered(&|value| value["content"]["ranges"][1]["file"] = json!(1)).is_err());
        assert!(
            tampered(&|value| value["content"]["other_files"]
                .as_array_mut()
                .unwrap()
                .push(c.clone()))
            .is_err()
        );
        let a = wire["content"]["file"].clone();
        assert!(tampered(&|value| value["content"]["other_files"] = json!([a.clone()])).is_err());
        // The untampered receipt still decodes.
        assert!(Receipt::from_json(&json).is_ok());
    }

    // AC-1024: Python receipts are admitted after those already held, within
    // the budget; one that does not fit is left out.
    #[test]
    fn python_receipts_are_admitted_after_held_ones_within_budget() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.py"), SOURCE).unwrap();
        let capture = source_capture::capture_working_tree(
            dir.path(),
            &source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap(),
            &["app.py".to_string()],
            source_capture::CaptureLimits::default(),
        )
        .unwrap();
        let id = SourceId {
            repo: "local/src_11111111111111111111111111111111",
            commit: "workdir",
        };
        let nodes = [
            node("sym:app.py#a", json!({ "prov": prov("app.py", (0, 3)) })),
            node("sym:app.py#b", json!({ "prov": prov("app.py", (4, 11)) })),
        ];
        let minted =
            mint_python_file_receipts(capture.file("app.py").unwrap(), &id, &nodes, &[]).unwrap();
        assert_eq!(minted.len(), 2);
        let mut held = vec![minted[0].clone()];
        admit_within(
            &mut held,
            vec![minted[1].clone()],
            ReceiptBudget {
                bytes: usize::MAX,
                receipts: 1,
            },
        );
        assert_eq!(held.len(), 1);
        admit_within(
            &mut held,
            vec![minted[1].clone()],
            ReceiptBudget {
                bytes: usize::MAX,
                receipts: 2,
            },
        );
        assert_eq!(held, minted);
        // Staging is bounded as candidates arrive.
        let mut stage = ReceiptStage::default();
        stage.extend_within(
            minted.clone(),
            ReceiptBudget {
                bytes: usize::MAX,
                receipts: 1,
            },
        );
        assert_eq!(stage.into_receipts(), minted[..1]);
    }
}
