//! Receipts for facts a manifest observation input enriched (ADR-0079,
//! AC-1045). Terraform state, Pulumi exports and OTLP traces are captured
//! under `input/<source id>` (ADR-0080); the T1 producers record the exact
//! byte span of each observation, so a receipt binds those spans of the
//! captured input. Like every lane receipt it binds primary provenance alone
//! and establishes no input closure.
use super::receipt::is_input_source;
use super::{CapturedError, MintedHeader, MintedReceipt, Receipt};
use crate::SourceId;
use core_graph::{Edge, Node};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The revision an input capture's evidence cites: a working-tree capture.
const INPUT_COMMIT: &str = "workdir";

/// Observed receipts for the `nodes` and `edges` whose observation cites
/// `capture`, an observation input capture: one per fact, minted against the
/// captured file the observation cites. A fact whose observation also cites
/// anything else, or whose spans do not lie in the capture, gets none.
pub fn mint_observed_receipts(
    capture: &source_capture::Capture,
    nodes: &[Node],
    edges: &[Edge],
) -> Vec<MintedReceipt> {
    let source = &capture.manifest().source_id;
    if !is_input_source(source) {
        return Vec::new();
    }
    let id = SourceId {
        repo: source.as_str(),
        commit: INPUT_COMMIT,
    };
    let mut headers: BTreeMap<String, Option<Arc<MintedHeader>>> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut receipts = Vec::new();
    let mut admit = |path: &str,
                     mint: &dyn Fn(
        &source_capture::CapturedFile,
    ) -> Result<Receipt, CapturedError>| {
        let Ok(file) = capture.file(path) else {
            return;
        };
        let header = headers.entry(path.to_owned()).or_default();
        let Ok(receipt) = mint(file).and_then(|receipt| MintedReceipt::compact(receipt, header))
        else {
            return;
        };
        if seen.insert((receipt.fact_key().clone(), receipt.fact_digest().to_owned())) {
            receipts.push(receipt);
        }
    };
    for node in nodes {
        if let Some(path) = observed_path(&node.props, id.repo) {
            admit(path, &|file| Receipt::for_node_in(file, None, &id, node));
        }
    }
    for edge in edges {
        if let Some(path) = observed_path(&edge.props, id.repo) {
            admit(path, &|file| Receipt::for_edge_in(file, None, &id, edge));
        }
    }
    receipts
}

/// The one captured input file a fact's observation cites: its
/// `observed_prov`, or its `prov` when the observation created the fact.
/// Every evidence entry must cite `repo` and that same file.
fn observed_path<'a>(props: &'a serde_json::Value, repo: &str) -> Option<&'a str> {
    let prov = props.get("observed_prov").or_else(|| props.get("prov"))?;
    let evidence = prov.get("evidence")?.as_array()?;
    let path = evidence.first()?.get("path")?.as_str()?;
    evidence
        .iter()
        .all(|entry| {
            entry.get("repo").and_then(serde_json::Value::as_str) == Some(repo)
                && entry.get("path").and_then(serde_json::Value::as_str) == Some(path)
        })
        .then_some(path)
}

#[cfg(test)]
mod tests {
    use super::super::Citable;
    use super::*;
    use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};

    const STATE: &[u8] =
        br#"{"values":{"root_module":{"resources":[{"address":"aws_sqs_queue.jobs"}]}}}"#;
    const INPUT: &str = "input/src_11111111111111111111111111111111";

    fn capture(source: &str) -> source_capture::Capture {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("state.json"), STATE).unwrap();
        source_capture::capture_working_tree(
            dir.path(),
            &source_capture::SourceId::new(source).unwrap(),
            &["state.json".into()],
            source_capture::CaptureLimits::default(),
        )
        .unwrap()
    }

    fn observed(repo: &str, path: &str, start: u64, end: u64) -> serde_json::Value {
        serde_json::to_value(
            Provenance::new(
                Tier::Dynamic,
                ConfidenceTier::Confirmed,
                vec![EvidenceRef {
                    repo: repo.into(),
                    path: path.into(),
                    byte_start: start,
                    byte_end: end,
                    commit_sha: "workdir".into(),
                }],
                "t1.terraform-state",
                b"Observed aws_sqs_queue.jobs",
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn resource(id: &str, observed_prov: serde_json::Value) -> Node {
        Node {
            id: id.into(),
            label: "Resource".into(),
            props: serde_json::json!({
                "prov": observed("local/src_11111111111111111111111111111111", "main.tf", 0, 1),
                "observed_prov": observed_prov,
            }),
        }
    }

    // AC-1045 (ADR-0079): a fact whose observation cites a captured input gets
    // one provenance-only receipt on that exact span, in the input's own
    // namespace; one citing another source, or not this capture, gets none.
    #[test]
    fn observed_receipts_bind_the_exact_input_span() {
        let capture = capture(INPUT);
        let start = STATE.windows(5).position(|w| w == b"{\"add").unwrap() as u64;
        let end = start + br#"{"address":"aws_sqs_queue.jobs"}"#.len() as u64;
        let bound = resource("res:a", observed(INPUT, "state.json", start, end));
        let elsewhere = resource("res:b", observed("acme/api", "state.json", start, end));
        let missing = resource("res:c", observed(INPUT, "other.json", start, end));
        let receipts = mint_observed_receipts(&capture, &[bound.clone(), elsewhere, missing], &[]);
        assert_eq!(receipts.len(), 1);
        let receipt = receipts[0].to_receipt().unwrap();
        receipt.validate().unwrap();
        assert!(receipt.id().starts_with("obs-input-v1:"));
        assert_eq!(receipt.repo_key(), INPUT);
        assert_eq!(receipt.source_id().as_str(), INPUT);
        assert!(receipts[0].matches_node(&bound));
        let span = receipt.captured_spans().next().unwrap();
        assert_eq!(
            capture.read_span(&span).unwrap(),
            &STATE[start as usize..end as usize]
        );
        assert_eq!(
            Receipt::from_json(&receipt.to_json().unwrap()).unwrap(),
            receipt
        );
    }

    // A capture of a registered source, not an input, mints no observed
    // receipt, and no registered receipt may claim the input namespace.
    #[test]
    fn only_input_captures_mint_observed_receipts() {
        let capture = capture("src_11111111111111111111111111111111");
        let start = 0;
        let node = resource("res:a", observed(INPUT, "state.json", start, 4));
        assert!(mint_observed_receipts(&capture, &[node], &[]).is_empty());
        let registered =
            source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap();
        assert!(super::super::receipt::validate_source_repo(&registered, INPUT).is_err());
        let input = source_capture::SourceId::new(INPUT).unwrap();
        assert!(super::super::receipt::validate_source_repo(&input, "acme/api").is_err());
        assert!(super::super::receipt::validate_source_repo(&input, INPUT).is_ok());
    }
}
