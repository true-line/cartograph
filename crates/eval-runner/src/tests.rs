// AC-1508: receipts are signed by a key that stays in the store, and only a
// receipt signed by the pinned key verifies.
use super::*;

#[test]
fn eval_runner_signs_receipts_with_a_store_only_key() {
    let store = tempfile::tempdir().unwrap();
    let public = keygen(store.path()).unwrap();
    assert_eq!(public.len(), 64);
    assert_eq!(public_key(store.path()).unwrap(), public);
    // The key is created once and is readable only by its owner.
    assert_eq!(keygen(store.path()), Err("key_exists_or_store_io"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(store.path().join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let signed = sign(
        store.path(),
        br#"{"purpose":"locked_independent_receipt"}"#.to_vec(),
    )
    .unwrap();
    assert_eq!(signed.public_key, public);
    assert!(verify(&signed, &public));

    // A changed receipt, a changed signature or another pinned key fails.
    let mut tampered = Signed {
        receipt: signed.receipt.replace("locked", "visible"),
        ..signed
    };
    assert!(!verify(&tampered, &public));
    tampered.receipt = r#"{"purpose":"locked_independent_receipt"}"#.into();
    assert!(verify(&tampered, &public));
    let mut flipped = tampered.signature.clone().into_bytes();
    flipped[0] = if flipped[0] == b'0' { b'1' } else { b'0' };
    tampered.signature = String::from_utf8(flipped).unwrap();
    assert!(!verify(&tampered, &public));

    let other = tempfile::tempdir().unwrap();
    let other_key = keygen(other.path()).unwrap();
    let signed = sign(store.path(), b"{}".to_vec()).unwrap();
    assert!(!verify(&signed, &other_key));
}

#[test]
fn eval_runner_refuses_without_a_key_or_model_config() {
    let store = tempfile::tempdir().unwrap();
    assert_eq!(
        sign(store.path(), b"{}".to_vec()).unwrap_err(),
        "invalid_store"
    );
    assert_eq!(model(store.path()), Err("invalid_store"));
    std::fs::write(
        store.path().join(MODEL_FILE),
        br#"{"schema_version":1,"model":"nomic-embed-text"}"#,
    )
    .unwrap();
    assert_eq!(model(store.path()).unwrap(), "nomic-embed-text");
    std::fs::write(
        store.path().join(MODEL_FILE),
        br#"{"schema_version":1,"model":"nomic-embed-text","url":"http://lan"}"#,
    )
    .unwrap();
    assert_eq!(model(store.path()), Err("invalid_store"));
}
