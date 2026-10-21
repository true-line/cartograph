use super::*;
use serde_json::json;
struct Case {
    _directory: tempfile::TempDir,
    root: PathBuf,
    manifest: PathBuf,
    proof: PathBuf,
}
impl Case {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source");
        std::fs::create_dir(&root).unwrap();
        let bytes = b"export const enabled = true;\n";
        std::fs::write(root.join("rule.ts"), bytes).unwrap();
        let manifest = directory.path().join("manifest.json");
        let proof = directory.path().join("proof.json");
        let body = json!({"schema_version":1,"repo":"example/control","commit_sha":"a".repeat(40),"scope":"Production source only","source_files":["rule.ts"]});
        let encoded = serde_json::to_vec(&body).unwrap();
        std::fs::write(&manifest, &encoded).unwrap();
        std::fs::write(&proof, serde_json::to_vec(&json!({"repo":"example/control","commit_sha":"a".repeat(40),"manifest_sha256":sha256(&encoded),"files_sha256":{"rule.ts":sha256(bytes)}})).unwrap()).unwrap();
        Self {
            _directory: directory,
            root,
            manifest,
            proof,
        }
    }
    fn verify(&self) -> Result<VerifiedInput, String> {
        verify(&self.root, &self.manifest, &self.proof)
    }
    fn change_manifest(&self, update: impl FnOnce(&mut serde_json::Value)) {
        let mut value = serde_json::from_slice(&std::fs::read(&self.manifest).unwrap()).unwrap();
        update(&mut value);
        let bytes = serde_json::to_vec(&value).unwrap();
        std::fs::write(&self.manifest, &bytes).unwrap();
        let mut proof: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&self.proof).unwrap()).unwrap();
        proof["manifest_sha256"] = json!(sha256(&bytes));
        std::fs::write(&self.proof, serde_json::to_vec(&proof).unwrap()).unwrap();
    }
}
#[test]
fn verified_input_binds_exact_bytes_in_both_hash_domains() {
    let case = Case::new();
    let first = case.verify().unwrap();
    assert_eq!(first, case.verify().unwrap());
    let bytes = std::fs::read(case.root.join("rule.ts")).unwrap();
    assert_eq!(first.files["rule.ts"].byte_len, bytes.len() as u64);
    assert_eq!(first.files["rule.ts"].sha256, sha256(&bytes));
    assert_eq!(
        first.files["rule.ts"].blake3,
        core_prov::content_hash(&bytes)
    );
    std::fs::write(
        case.root.join("rule.ts"),
        b"export const enabled = false;\n",
    )
    .unwrap();
    assert!(case.verify().is_err());
}
#[test]
fn verified_input_rejects_missing_extra_and_evaluator_documents() {
    for mutation in ["missing", "extra", "oracle", "manifest_inside"] {
        let case = Case::new();
        match mutation {
            "missing" => std::fs::remove_file(case.root.join("rule.ts")).unwrap(),
            "extra" => std::fs::write(case.root.join("unexpected.ts"), b"extra").unwrap(),
            "oracle" => {
                std::fs::write(case.root.join("expected.json"), b"withheld answer").unwrap()
            }
            _ => {
                std::fs::copy(&case.manifest, case.root.join("manifest.json")).unwrap();
            }
        }
        assert!(case.verify().is_err(), "{mutation}");
        if mutation == "manifest_inside" {
            assert!(verify(&case.root, &case.root.join("manifest.json"), &case.proof).is_err());
        }
    }
}
#[test]
fn verified_input_rejects_duplicate_json_paths_traversal_and_unknown_fields() {
    for mutation in [
        "duplicate_key",
        "duplicate_path",
        "traversal",
        "absolute",
        "unknown",
    ] {
        let case = Case::new();
        if mutation == "duplicate_key" {
            let bytes = std::fs::read_to_string(&case.proof).unwrap();
            std::fs::write(
                &case.proof,
                bytes.replacen('{', "{\"repo\":\"example/control\",", 1),
            )
            .unwrap();
        } else {
            case.change_manifest(|body| match mutation {
                "duplicate_path" => body["source_files"] = json!(["rule.ts", "rule.ts"]),
                "traversal" => body["source_files"] = json!(["../rule.ts"]),
                "absolute" => body["source_files"] = json!(["/rule.ts"]),
                _ => body["unknown"] = json!(true),
            });
        }
        assert!(case.verify().is_err(), "{mutation}");
    }
}
#[cfg(unix)]
#[test]
fn verified_input_rejects_file_and_directory_symlinks() {
    for directory in [false, true] {
        let case = Case::new();
        if directory {
            std::os::unix::fs::symlink(case.root.parent().unwrap(), case.root.join("alias"))
                .unwrap();
        } else {
            let original = case.root.join("rule.ts");
            let outside = case.root.parent().unwrap().join("outside.ts");
            std::fs::rename(&original, &outside).unwrap();
            std::os::unix::fs::symlink(outside, original).unwrap();
        }
        assert!(case.verify().is_err());
    }
}
#[test]
fn verified_input_bounds_reads_before_hashing() {
    let case = Case::new();
    std::fs::File::create(case.root.join("rule.ts"))
        .unwrap()
        .set_len(MAX_FILE as u64 + 1)
        .unwrap();
    assert!(case.verify().is_err());
    let case = Case::new();
    std::fs::File::create(&case.proof)
        .unwrap()
        .set_len(MAX_DOCUMENT as u64 + 1)
        .unwrap();
    assert!(case.verify().is_err());
}
