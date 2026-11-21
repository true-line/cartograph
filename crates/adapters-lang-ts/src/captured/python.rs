//! Provenance-only receipts for the captured Python (AC-1024), Go (AC-1026),
//! Java (AC-1031), Rust (AC-1033) and Kotlin (AC-1035) lanes (#385).
//!
//! Each adapter parses a captured `.py`, `.go`, `.java`, `.rs`, `.kt` or
//! `.kts` file's retained bytes; this module binds a lane's facts to the exact
//! captured spans their primary provenance cites. Rules and facts citing more
//! than one file get none. Facts are bound in their final form, after
//! directory resolution (AC-1037), so those the directory join builds or
//! rewrites are covered too; receipts are admitted after every TypeScript
//! receipt within the same extraction budget.

use super::{
    CapturedError, Charge, Grammar, MAX_EXTRACTION_RECEIPT_BYTES, MAX_EXTRACTION_RECEIPTS,
    MintedReceipt, Receipt, ReceiptBudget, SourceId, valid_path,
};
use core_graph::{Edge, Node};
use source_capture::CapturedFile;

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
    let path = &file.reference().path;
    if Grammar::for_path(path) != grammar || !valid_path(path) {
        return Err(CapturedError::Invalid("unsupported captured path"));
    }
    super::receipt::validate_identity(file.reference(), id.repo)?;
    let mut header = None;
    let mut receipts = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let minted = nodes
        .iter()
        .map(|node| Receipt::for_node(file, id, node))
        .chain(edges.iter().map(|edge| Receipt::for_edge(file, id, edge)));
    for receipt in minted {
        let Ok(receipt) = receipt.and_then(|receipt| MintedReceipt::compact(receipt, &mut header))
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

/// Receipt candidates for a lane's final facts, after directory resolution
/// (AC-1037). Each fact whose primary provenance lies wholly in one of the
/// captured `files` is minted against that file, file by file in `files`
/// order, and staged within the extraction budget; a fact citing several
/// files, or a file outside the capture, gets none.
pub fn stage_lane_receipts(
    grammar: Grammar,
    capture: &source_capture::Capture,
    files: &[String],
    id: &SourceId,
    nodes: &[Node],
    edges: &[Edge],
) -> ReceiptStage {
    let mut by_file: std::collections::BTreeMap<&str, (Vec<&Node>, Vec<&Edge>)> =
        std::collections::BTreeMap::new();
    for node in nodes {
        if let Some(path) = sole_evidence_path(&node.props) {
            by_file.entry(path).or_default().0.push(node);
        }
    }
    for edge in edges {
        if let Some(path) = sole_evidence_path(&edge.props) {
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
        if let Ok(receipts) = mint_lane_file_receipts(grammar, file, id, &nodes, &edges) {
            stage.extend(receipts);
        }
    }
    stage
}

/// The one file every primary evidence entry of a fact cites, if there is one.
fn sole_evidence_path(props: &serde_json::Value) -> Option<&str> {
    let evidence = props.get("prov")?.get("evidence")?.as_array()?;
    let mut paths = evidence
        .iter()
        .map(|entry| entry.get("path").and_then(serde_json::Value::as_str));
    let first = paths.next()??;
    paths.all(|path| path == Some(first)).then_some(first)
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
    // order; a fact citing two files, or a file outside the capture, gets none.
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
                "ep:cross",
                json!({ "prov": prov_spanning(&[("a.py", (0, 3)), ("b.py", (0, 3))]) }),
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
