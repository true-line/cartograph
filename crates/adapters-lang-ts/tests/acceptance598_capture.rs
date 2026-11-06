//! Provider-free fixture transport witness. No model or official profile access.
use adapters_lang_ts::{SourceId, captured::extract_file};
use source_capture::{
    CaptureLimits, CaptureStore, SourceId as CaptureSourceId, StoreLimits, capture_working_tree,
};
use std::fs;

const INPUTS: &[(&str, &str)] = &[
    (
        "ambiguity/routing.ts",
        include_str!("fixtures/acceptance598/ambiguity/routing.ts"),
    ),
    (
        "missing_input/refunds.ts",
        include_str!("fixtures/acceptance598/missing_input/refunds.ts"),
    ),
    (
        "conflicting_evidence/observation-a.ts",
        include_str!("fixtures/acceptance598/conflicting_evidence/observation-a.ts"),
    ),
    (
        "conflicting_evidence/observation-b.ts",
        include_str!("fixtures/acceptance598/conflicting_evidence/observation-b.ts"),
    ),
    (
        "stale_evidence/deployment-history.ts",
        include_str!("fixtures/acceptance598/stale_evidence/deployment-history.ts"),
    ),
    (
        "injection/routing.ts",
        include_str!("fixtures/acceptance598/injection/routing.ts"),
    ),
];

#[test]
fn acceptance598_complete_evidence_survives_checkout_change_and_removal() {
    // Follow manifest case grouping: contradictory observations share one capture.
    // This is a transport assertion, never a model robustness score.
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/acceptance598/manifest.json")).unwrap();
    let cases = manifest["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 5);
    let mut checked = std::collections::BTreeSet::new();
    for case in cases {
        let scratch = tempfile::tempdir().unwrap();
        let checkout = scratch.path().join("checkout");
        let mut paths = Vec::new();
        for input in case["inputs"].as_array().unwrap() {
            let path = input["path"].as_str().unwrap();
            assert!(checked.insert(path), "duplicate manifest input");
            let source = INPUTS.iter().find(|(p, _)| *p == path).unwrap().1;
            assert_eq!(source.len() as u64, input["bytes"].as_u64().unwrap());
            let file = checkout.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, source).unwrap();
            paths.push(path.to_owned());
        }
        let id = CaptureSourceId::new("src_11111111111111111111111111111111").unwrap();
        let capture =
            capture_working_tree(&checkout, &id, &paths, CaptureLimits::default()).unwrap();
        let database = scratch.path().join("captures.sqlite");
        let mut store = CaptureStore::open(&database, StoreLimits::default()).unwrap();
        store.persist(&capture).unwrap();
        let mut retained = Vec::new();
        for input in case["inputs"].as_array().unwrap() {
            let path = input["path"].as_str().unwrap();
            let source = INPUTS.iter().find(|(p, _)| *p == path).unwrap().1;
            let start = input["evidence_byte_range"][0].as_u64().unwrap() as usize;
            let end = input["evidence_byte_range"][1].as_u64().unwrap() as usize;
            let evidence = &source[start..end];
            assert!(!evidence.is_empty());
            assert_eq!(
                evidence,
                source
                    .split_once("return `")
                    .unwrap()
                    .1
                    .split_once("`;")
                    .unwrap()
                    .0
            );
            let (_, receipts) = extract_file(
                capture.file(path).unwrap(),
                &SourceId {
                    repo: "local/src_11111111111111111111111111111111",
                    commit: "workdir",
                },
            )
            .unwrap();
            let range = receipts
                .iter()
                .flat_map(|receipt| receipt.ranges())
                .find(|range| {
                    capture
                        .read_text_span(&range.captured)
                        .unwrap()
                        .contains(evidence)
                })
                .unwrap_or_else(|| panic!("full evidence has no producer-issued span: {path}"));
            let reference = range.captured.clone();
            let expected = capture.read_span(&reference).unwrap().to_vec();
            retained.push((reference, expected));
        }
        drop(capture);
        for path in &paths {
            fs::write(checkout.join(path), "export const replacement = true;\n").unwrap();
        }
        for (reference, expected) in &retained {
            assert_eq!(&store.read_span(reference).unwrap(), expected);
        }
        fs::remove_dir_all(&checkout).unwrap();
        drop(store);
        let mut store = CaptureStore::open(&database, StoreLimits::default()).unwrap();
        for (reference, expected) in &retained {
            assert_eq!(&store.read_span(reference).unwrap(), expected);
        }
        // Only this case's disposable source may be forgotten.
        let mut ids: Vec<_> = store
            .source_inventory(&id)
            .unwrap()
            .into_iter()
            .map(|x| x.capture_id)
            .collect();
        ids.sort();
        assert_eq!(store.forget_source(&id, &ids).unwrap(), 1);
        for (reference, _) in &retained {
            assert!(matches!(
                store.read_span(reference),
                Err(source_capture::CaptureError::Missing(_))
            ));
        }
        println!(
            "{}: {} inputs retained together through mutation/removal/reopen; forgotten spans unavailable",
            case["id"].as_str().unwrap(),
            paths.len()
        );
    }
    assert_eq!(checked.len(), INPUTS.len(), "fixture omitted from manifest");
}
