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
