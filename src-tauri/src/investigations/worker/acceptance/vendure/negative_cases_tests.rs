//! Non-evidence transport witnesses: synthetic local sources, no model calls.
use super::*;
use std::path::PathBuf;
pub(in super::super::super) struct Fixture {
    _temp: tempfile::TempDir,
    pub base: PathBuf,
    pub source: PathBuf,
    pub config: PathBuf,
    pub manifest: PathBuf,
    pub proof: PathBuf,
}
impl Fixture {
    pub(in super::super::super) fn new(case: NegativeCase598) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = crate::paths::canonicalize(temp.path()).unwrap();
        let source = base.join("source");
        for file in case.spec().unwrap().inputs {
            let path = source.join(&file.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source_bytes(&file.path).unwrap()).unwrap();
        }
        let (manifest, proof) = case.documents().unwrap();
        let paths = (base.join("input.json"), base.join("proof.json"));
        std::fs::write(&paths.0, serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::write(&paths.1, serde_json::to_vec(&proof).unwrap()).unwrap();
        let profile = base.join("private");
        let config = base.join("config.json");
        std::fs::write(&config,serde_json::to_vec(&json!({"productName":"Cartograph Acceptance 598 Fixture",
            "identifier":"com.qwtm.cartograph.acceptance598.fixture",
            "app":{"appDirectoriesOverride":{"data":profile.join("data"),"config":profile.join("config"),
            "localData":profile.join("local-data"),"cache":profile.join("cache"),"log":profile.join("logs")}}})).unwrap()).unwrap();
        Self {
            _temp: temp,
            base,
            source,
            config,
            manifest: paths.0,
            proof: paths.1,
        }
    }
    pub(in super::super::super) fn prepare(&self, case: NegativeCase598) -> Result<Value, String> {
        let output = self.base.join("capture-report");
        std::fs::create_dir(&output).map_err(|_| "capture output exists")?;
        let prepared = capture(
            case,
            &self.source,
            &self.manifest,
            &self.proof,
            &self.config,
            "a".repeat(40),
            &output,
        )?;
        serde_json::to_value(prepared).map_err(|_| "preparation encoding".into())
    }
}
// AC-0589: closed fixture transport only, never semantic acceptance evidence.
#[test]
fn negative_fixture_binding_rejects_drift_before_profile_creation() {
    for id in [
        "ambiguity",
        "missing_input",
        "conflicting_evidence",
        "stale_evidence",
        "injection",
    ] {
        let case = NegativeCase598::parse(id).unwrap();
        let fixture = Fixture::new(case);
        let input = input::verify(&fixture.source, &fixture.manifest, &fixture.proof).unwrap();
        case.verify(&input).unwrap();
        assert!(verify_frozen_input(&input).is_err()); // FrozenVendure unchanged
        let mut forged = input.clone();
        forged.proof_sha256 = "0".repeat(64);
        assert!(case.verify(&forged).is_err());
        let file = case.spec().unwrap().inputs.remove(0);
        std::fs::write(fixture.source.join(file.path), b"changed source").unwrap();
        assert!(fixture.prepare(case).is_err());
        assert!(!fixture.base.join("private").exists());
    }
    let case = NegativeCase598::Ambiguity;
    let fixture = Fixture::new(case);
    std::fs::write(fixture.source.join("evaluator.json"), b"{}").unwrap();
    assert!(fixture.prepare(case).is_err());
    assert!(!fixture.base.join("private").exists());
    assert!(NegativeCase598::parse("arbitrary").is_err());
}
