//! Provenance-only receipts for the captured Python lane (AC-1023, #385).
//!
//! The Python adapter parses each captured `.py` file's retained bytes; this
//! module binds that file's own facts to the exact captured spans their
//! primary provenance cites. Rules and facts citing another file get none.
//! Receipts are kept only for facts that survive directory resolution
//! unchanged, and are admitted after every TypeScript receipt within the same
//! extraction budget.

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
    let path = &file.reference().path;
    if Grammar::for_path(path) != Grammar::Python || !valid_path(path) {
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

/// Admit `candidates` after `held` within the extraction receipt budget, in
/// order; one that does not fit is left out and its fact stays uncaptured.
pub fn admit_after(held: &mut Vec<MintedReceipt>, candidates: Vec<MintedReceipt>) {
    admit_within(
        held,
        candidates,
        ReceiptBudget {
            bytes: MAX_EXTRACTION_RECEIPT_BYTES,
            receipts: MAX_EXTRACTION_RECEIPTS,
        },
    )
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

    // AC-1023: a Python fact citing its own captured file gets a Python
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
        assert!(mint_python_file_receipts(ts.file("a.ts").unwrap(), &id, &[own], &[]).is_err());
    }

    // AC-1023: Python receipts are admitted after those already held, within
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
    }
}
