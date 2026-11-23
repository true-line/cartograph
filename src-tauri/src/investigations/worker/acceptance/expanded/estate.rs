//! #1725 side experiment only: stage and execute the captured local analyst over
//! one pinned OSS estate closure, reusing the Vendure captured-local path with
//! `InputPolicy::PinnedEstate`. Ignored manual tests; no product behavior.
use super::super::vendure;
use super::{captured_local, vendure_prepare};
use std::path::PathBuf;

fn path(name: &str) -> PathBuf {
    PathBuf::from(super::super::required(name))
}

fn definition(path: &std::path::Path) -> vendure_prepare::Definition {
    agents::investigation::decode_record(
        std::str::from_utf8(&vendure::read_preparation_artifact(path, 128 * 1024).unwrap())
            .unwrap(),
    )
    .unwrap()
}

#[test]
#[ignore = "manual #1725 estate analyst staging; no provider"]
fn estate_prepare_captured_analyst() {
    let definition_path = path("CARTOGRAPH_ESTATE_ANALYST_DEFINITION");
    let expected = definition(&definition_path);
    vendure_prepare::prepare_bound(
        &path("CARTOGRAPH_ESTATE_NATIVE_CONFIG"),
        &path("CARTOGRAPH_ESTATE_PREPARATION_RECEIPT"),
        &definition_path,
        &path("CARTOGRAPH_ESTATE_ANALYST_OUTPUT"),
        &expected,
        vendure::InputPolicy::PinnedEstate,
    )
    .unwrap();
}

#[test]
#[ignore = "manual #1725 estate local analyst; requires explicit local provider variables"]
fn estate_captured_local_analyst() {
    let definition_path = path("CARTOGRAPH_ESTATE_ANALYST_DEFINITION");
    let mut expected = definition(&definition_path);
    expected.start_sha256 = Some(super::super::required(
        "CARTOGRAPH_ESTATE_ANALYST_START_SHA256",
    ));
    let env = |name: &str| std::env::var(name).ok();
    captured_local::analyst_entry(
        [
            &path("CARTOGRAPH_ESTATE_NATIVE_CONFIG"),
            &path("CARTOGRAPH_ESTATE_PREPARATION_RECEIPT"),
            &definition_path,
            &path("CARTOGRAPH_ESTATE_ANALYST_OUTPUT"),
        ],
        &expected,
        vendure::InputPolicy::PinnedEstate,
        true,
        &env,
    )
    .unwrap();
}

/// #1728 selection rule v3 (AC-1433): print the top seven rule-bearing source
/// selections for one prepared estate profile: `selections` holds entries for
/// the analyst definition and `ranking` the counts behind each. Point CARTOGRAPH_ESTATE_PROFILE_DATA at a copy of the
/// profile's data directory (opening it runs the normal store migrations) and
/// CARTOGRAPH_ESTATE_CLOSURE at a JSON list of the question's closure paths.
/// Rule v3 fills any shortfall in rule v2 order outside this test.
#[test]
#[ignore = "manual #1728 rule v3 selection; reads a copied profile, no provider"]
fn estate_rule_bearing_selections() {
    let data = path("CARTOGRAPH_ESTATE_PROFILE_DATA");
    let closure: Vec<String> =
        serde_json::from_slice(&std::fs::read(path("CARTOGRAPH_ESTATE_CLOSURE")).unwrap()).unwrap();
    let files = closure.into_iter().collect();
    let state = crate::registered_source_tests::app_state(&data);
    let sources =
        crate::investigations::rule_sources::rule_bearing_sources(&state, Some(&files), 7).unwrap();
    let selections: Vec<_> = sources
        .iter()
        .map(|source| {
            serde_json::json!({
                "fact": source.fact,
                "graph_snapshot_id": source.graph_snapshot_id,
                "receipt_id": source.receipt_id,
                "ordinal": source.range_index,
            })
        })
        .collect();
    let ranking: Vec<_> = sources.iter().map(|source| &source.owner).collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "selections": selections,
            "ranking": ranking,
        }))
        .unwrap()
    );
}
