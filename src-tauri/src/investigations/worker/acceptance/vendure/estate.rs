//! #1725 side experiment only: capture preparation for one pinned OSS estate
//! closure. The same production ingest as `vendure_capture_preparation`, bound by
//! the closure's own manifest and proof instead of the frozen Vendure identity.
//! No provider calls; no product behavior.
use super::*;

#[test]
#[ignore = "manual #1725 estate capture preparation; no provider calls"]
fn estate_capture_preparation() {
    let required = |name| std::path::PathBuf::from(super::super::required(name));
    let source = required("CARTOGRAPH_ESTATE_SOURCE");
    let manifest = required("CARTOGRAPH_ESTATE_MANIFEST");
    let proof = required("CARTOGRAPH_ESTATE_PROOF");
    let config = required("CARTOGRAPH_ESTATE_NATIVE_CONFIG");
    let output = required("CARTOGRAPH_ESTATE_PREPARATION_OUTPUT");
    assert!(output.is_absolute());
    let source_root = crate::paths::canonicalize(&source).unwrap();
    let parent = crate::paths::canonicalize(output.parent().unwrap()).unwrap();
    assert_eq!(output.parent().unwrap(), parent);
    assert!(!output.starts_with(&source_root));
    let (profile, config_hash) = profile::NativeProfile::read(&config).unwrap();
    validate_paths(&source, &manifest, &proof, &config, &profile, &output).unwrap();
    std::fs::create_dir(&output).expect("Preparation output must be new");
    let output_guard = OutputDirectory::pin(&output).unwrap();
    let revision = super::super::source_revision();
    let result = prepare(
        &source, &manifest, &proof, &config, revision, &output, false,
    );
    validate_paths(&source, &manifest, &proof, &config, &profile, &output).unwrap();
    verify_config(&config, &config_hash).unwrap();
    match result {
        Ok(preparation) => output_guard
            .write("preparation.json", &preparation)
            .unwrap(),
        Err(error) => {
            output_guard
                .write(
                    "preparation-failed.json",
                    &serde_json::json!({"schema_version":1,"status":"preparation_failed",
                    "provider_calls":0,"acceptance_passed":false,"reason":error}),
                )
                .unwrap();
            panic!("estate preparation failed; no provider run occurred");
        }
    }
}
