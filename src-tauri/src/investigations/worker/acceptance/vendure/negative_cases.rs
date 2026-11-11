//! Closed #598 synthetic transport fixture. Evaluator documents are never source.
//! This is unrelated to FrozenVendure and cannot admit arbitrary fixture inputs.
use super::*;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(in super::super) const MANIFEST_SHA: &str =
    "cb54f13de62cdb6fdb4f97ffafd1a5e8a8f8d3999b9a16aaa9606ef513745092";
const MANIFEST: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../crates/adapters-lang-ts/tests/fixtures/acceptance598/manifest.json"
));

#[derive(Clone, Copy, Debug)]
pub(in super::super) enum NegativeCase598 {
    Ambiguity,
    MissingInput,
    ConflictingEvidence,
    StaleEvidence,
    Injection,
}
#[derive(Deserialize)]
pub(in super::super) struct Spec {
    pub id: String,
    pub question: String,
    pub inputs: Vec<Source>,
}
#[derive(Deserialize)]
pub(in super::super) struct Source {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub evidence_byte_range: [u64; 2],
}
pub(in super::super) fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
impl NegativeCase598 {
    pub(in super::super) fn parse(id: &str) -> Result<Self, String> {
        match id {
            "ambiguity" => Ok(Self::Ambiguity),
            "missing_input" => Ok(Self::MissingInput),
            "conflicting_evidence" => Ok(Self::ConflictingEvidence),
            "stale_evidence" => Ok(Self::StaleEvidence),
            "injection" => Ok(Self::Injection),
            _ => Err("unknown negative case".into()),
        }
    }
    pub(in super::super) fn id(self) -> &'static str {
        match self {
            Self::Ambiguity => "ambiguity",
            Self::MissingInput => "missing_input",
            Self::ConflictingEvidence => "conflicting_evidence",
            Self::StaleEvidence => "stale_evidence",
            Self::Injection => "injection",
        }
    }
    pub(in super::super) fn spec(self) -> Result<Spec, String> {
        if sha(MANIFEST) != MANIFEST_SHA {
            return Err("compiled manifest drift".into());
        }
        let manifest: Value = serde_json::from_slice(MANIFEST).map_err(|_| "manifest schema")?;
        let item = manifest["cases"]
            .as_array()
            .ok_or("manifest cases")?
            .iter()
            .find(|c| c["id"] == self.id())
            .ok_or("manifest case missing")?;
        serde_json::from_value(item.clone()).map_err(|_| "manifest case schema".into())
    }
    /// Deterministic transport documents, not a Git provenance claim: commit_sha
    /// is a synthetic fixture revision (manifest digest prefix), explicitly scoped.
    pub(in super::super) fn documents(self) -> Result<(Value, Value), String> {
        let spec = self.spec()?;
        let repo = format!("fixture/acceptance598/{}", self.id());
        let revision = &MANIFEST_SHA[..40];
        let manifest = json!({"schema_version":1,"repo":repo,"commit_sha":revision,
            "scope":"synthetic negative case; revision is manifest digest prefix, not Git",
            "source_files":spec.inputs.iter().map(|f| &f.path).collect::<Vec<_>>()});
        let proof = json!({"repo":repo,"commit_sha":revision,
            "manifest_sha256":sha(&serde_json::to_vec(&manifest).map_err(|_| "manifest encoding")?),
            "files_sha256":spec.inputs.iter().map(|f| (f.path.clone(),f.sha256.clone())).collect::<BTreeMap<_,_>>()});
        Ok((manifest, proof))
    }
    fn expected(self) -> Result<input::VerifiedInput, String> {
        let (manifest, proof) = self.documents()?;
        let mut files = BTreeMap::new();
        for f in self.spec()?.inputs {
            let bytes = source_bytes(&f.path)?;
            if sha(bytes) != f.sha256 || bytes.len() as u64 != f.bytes {
                return Err("compiled source drift".into());
            }
            files.insert(
                f.path,
                input::VerifiedFile {
                    byte_len: f.bytes,
                    sha256: f.sha256,
                    blake3: core_prov::content_hash(bytes),
                },
            );
        }
        Ok(input::VerifiedInput {
            repo: manifest["repo"].as_str().unwrap().into(),
            commit_sha: MANIFEST_SHA[..40].into(),
            manifest_sha256: sha(&serde_json::to_vec(&manifest).map_err(|_| "manifest encoding")?),
            proof_sha256: sha(&serde_json::to_vec(&proof).map_err(|_| "proof encoding")?),
            files,
        })
    }
    pub(super) fn verify(self, input: &input::VerifiedInput) -> Result<(), String> {
        if *input != self.expected()? {
            return Err("negative fixture input binding mismatch".into());
        }
        Ok(())
    }
    pub(in super::super) fn binding(self) -> Result<Value, String> {
        let spec = self.spec()?;
        Ok(
            json!({"schema":"cartograph-negative-case-binding@1","case":spec.id,
            "manifest_sha256":MANIFEST_SHA,"question_sha256":sha(spec.question.as_bytes())}),
        )
    }
}
fn source_bytes(path: &str) -> Result<&'static [u8], String> {
    macro_rules! file {
        ($p:literal) => {
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../crates/adapters-lang-ts/tests/fixtures/acceptance598/",
                $p
            ))
            .as_slice()
        };
    }
    match path {
        "ambiguity/routing.ts" => Ok(file!("ambiguity/routing.ts")),
        "missing_input/refunds.ts" => Ok(file!("missing_input/refunds.ts")),
        "conflicting_evidence/observation-a.ts" => {
            Ok(file!("conflicting_evidence/observation-a.ts"))
        }
        "conflicting_evidence/observation-b.ts" => {
            Ok(file!("conflicting_evidence/observation-b.ts"))
        }
        "stale_evidence/deployment-history.ts" => Ok(file!("stale_evidence/deployment-history.ts")),
        "injection/routing.ts" => Ok(file!("injection/routing.ts")),
        _ => Err("source outside negative fixture".into()),
    }
}

#[allow(clippy::too_many_arguments)]
fn capture(
    case: NegativeCase598,
    source: &Path,
    manifest: &Path,
    proof: &Path,
    config: &Path,
    revision: String,
    output: &Path,
) -> Result<Preparation, String> {
    // Fail before profile creation, registration, capture or admission. The
    // normal verifier also rejects extra evaluator files and symlink sources.
    case.verify(&input::verify(source, manifest, proof)?)?;
    let prepared = prepare_fixture(source, manifest, proof, config, revision, output)?;
    case.verify(&prepared.input)?;
    Ok(prepared)
}

#[test]
#[ignore = "manual fixture capture only; no provider or semantic evidence"]
fn negative_case_capture_preparation() {
    let var = |name: &str| std::env::var(name).expect("required negative capture variable");
    let case = NegativeCase598::parse(&var("CARTOGRAPH_NEGATIVE_CASE")).unwrap();
    assert_eq!(var("CARTOGRAPH_NEGATIVE_MANIFEST_SHA256"), MANIFEST_SHA);
    let prepared = capture(
        case,
        Path::new(&var("CARTOGRAPH_ACCEPTANCE_SOURCE")),
        Path::new(&var("CARTOGRAPH_ACCEPTANCE_MANIFEST")),
        Path::new(&var("CARTOGRAPH_ACCEPTANCE_PROOF")),
        Path::new(&var("CARTOGRAPH_ACCEPTANCE_CONFIG")),
        var("CARTOGRAPH_ACCEPTANCE_REVISION"),
        Path::new(&var("CARTOGRAPH_ACCEPTANCE_OUTPUT")),
    )
    .unwrap();
    report::write_new(
        &Path::new(&var("CARTOGRAPH_ACCEPTANCE_OUTPUT")).join("preparation.json"),
        &prepared,
    )
    .unwrap();
}

#[cfg(test)]
#[path = "negative_cases_tests.rs"]
pub(in super::super) mod tests;
