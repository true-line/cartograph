use super::*;
use serde_json::json;

// AC-0191: crash/unknown outcomes must never silently become a fresh attempt.
#[test]
fn admission_artifacts_are_exclusive_and_tampering_fails_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("attempt.json");
    let value = json!({"family":"admission@10", "preparation_hash":"a".repeat(64)});
    let reference = write_new(&path, &value).unwrap();
    let original = read_verified(&path, &reference).unwrap();
    assert!(write_new(&path, &json!({"replacement":true})).is_err());
    assert_eq!(read_verified(&path, &reference).unwrap(), original);
    let mut changed = original.clone();
    changed[0] = b'[';
    std::fs::write(&path, changed).unwrap();
    assert!(read_verified(&path, &reference).is_err());
    // A partial artifact left after an uncertain write blocks replay too.
    std::fs::write(&path, b"{").unwrap();
    assert!(write_new(&path, &value).is_err());
    assert!(read_verified(&path, &reference).is_err());
}

// AC-0191: reject over-capacity artifacts before any persistent publication.
#[test]
fn admission_artifacts_bound_serialization_and_reference_before_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("receipt.json");
    assert!(write_new(&path, &"x".repeat(MAX_ARTIFACT_BYTES)).is_err());
    assert!(!path.exists());
    let value = "x".repeat(MAX_ARTIFACT_BYTES - 2); // JSON quotation bytes.
    let reference = write_new(&path, &value).unwrap();
    assert_eq!(reference.byte_len, MAX_ARTIFACT_BYTES as u64);
    assert_eq!(
        read_verified(&path, &reference).unwrap().len(),
        MAX_ARTIFACT_BYTES
    );
    let mut invalid = reference;
    invalid.byte_len += 1;
    assert!(read_verified(&path, &invalid).is_err());
    invalid.byte_len -= 1;
    invalid.sha256 = "G".repeat(64);
    assert!(read_verified(&path, &invalid).is_err());
}

#[cfg(unix)]
#[test]
fn admission_artifacts_reject_symlinks_and_do_not_replace_their_targets() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.json");
    let reference = write_new(&target, &json!({"saved":true})).unwrap();
    let link = dir.path().join("link.json");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(read_verified(&link, &reference).is_err());
    assert!(write_new(&link, &json!({"replacement":true})).is_err());
    assert!(read_verified(&target, &reference).is_ok());
}
