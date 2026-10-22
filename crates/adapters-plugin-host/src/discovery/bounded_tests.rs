use super::*;
use std::io::Cursor;

fn limits() -> DiscoveryLimits {
    DiscoveryLimits {
        max_entries: 100,
        max_file_bytes: 100_000,
        max_metadata_bytes: 100_000,
    }
}
fn write(dir: &Path, name: &str, bytes: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), bytes).unwrap();
}

#[test]
fn bounded_discovery_matches_legacy_hashes_and_sorted_project_precedence() {
    // AC-0288: bounded metadata discovery preserves existing plugin identity.
    let temp = tempfile::tempdir().unwrap();
    let user = temp.path().join("user");
    let first = temp.path().join("a");
    let second = temp.path().join("z");
    write(&user, "ruby.wasm", b"user");
    write(&user, "kotlin.wasm", b"kotlin");
    write(&first.join(PROJECT_ADAPTER_DIR), "ruby.wasm", b"first");
    write(&second.join(PROJECT_ADAPTER_DIR), "ruby.wasm", b"second");
    let roots = vec![second, first];
    let actual = discover_bounded(&roots, &user, limits()).unwrap();
    assert_eq!(
        serde_json::to_value(&actual).unwrap(),
        serde_json::to_value(super::super::discover(&roots, &user)).unwrap()
    );
    assert_eq!(actual[1].content_hash, core_prov::content_hash(b"first"));
    assert!(actual[1].shadowed_user_copy);
}

#[test]
fn bounded_discovery_counts_ignored_entries_and_aggregate_artifact_bytes() {
    let temp = tempfile::tempdir().unwrap();
    write(temp.path(), "a.wasm", b"abc");
    write(temp.path(), "b.wasm", b"defg");
    write(temp.path(), "ignored.txt", b"not read");
    let mut cap = limits();
    cap.max_entries = 3;
    cap.max_file_bytes = 7;
    assert_eq!(discover_bounded(&[], temp.path(), cap).unwrap().len(), 2);
    cap.max_entries = 2;
    assert_eq!(
        discover_bounded(&[], temp.path(), cap).unwrap_err(),
        DiscoveryError::EntryLimit
    );
    cap.max_entries = 3;
    cap.max_file_bytes = 6;
    assert_eq!(
        discover_bounded(&[], temp.path(), cap).unwrap_err(),
        DiscoveryError::FileByteLimit
    );
}

#[test]
fn bounded_discovery_metadata_exact_boundary_includes_ignored_names_and_paths() {
    let temp = tempfile::tempdir().unwrap();
    write(temp.path(), "café.txt", b"ignored");
    let dir_len = temp.path().as_os_str().as_encoded_bytes().len();
    let name_len = std::ffi::OsStr::new("café.txt").as_encoded_bytes().len();
    let total = dir_len + 1 + name_len + 1 + dir_len + name_len + 1 + 1;
    let mut cap = limits();
    cap.max_metadata_bytes = total;
    assert!(discover_bounded(&[], temp.path(), cap).unwrap().is_empty());
    cap.max_metadata_bytes -= 1;
    assert_eq!(
        discover_bounded(&[], temp.path(), cap).unwrap_err(),
        DiscoveryError::MetadataLimit
    );
}

#[test]
fn bounded_discovery_missing_dirs_are_empty_and_other_io_is_not_omitted() {
    let temp = tempfile::tempdir().unwrap();
    assert!(
        discover_bounded(
            &[temp.path().join("missing-project")],
            &temp.path().join("missing-user"),
            limits()
        )
        .unwrap()
        .is_empty()
    );
    let file = temp.path().join("not-a-directory");
    std::fs::write(&file, b"file").unwrap();
    assert_eq!(
        discover_bounded(&[], &file, limits()).unwrap_err(),
        DiscoveryError::Io
    );
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("fixture"))
        }
    }
    assert_eq!(
        hash_reader(&mut Broken, &mut 100).unwrap_err(),
        DiscoveryError::Io
    );
}

#[test]
fn bounded_discovery_rejects_sparse_huge_artifact_without_reading_it() {
    let temp = tempfile::tempdir().unwrap();
    File::create(temp.path().join("huge.wasm"))
        .unwrap()
        .set_len(if cfg!(unix) {
            1_u64 << 40
        } else {
            16 * 1024 * 1024
        })
        .unwrap();
    let mut cap = limits();
    cap.max_file_bytes = 1;
    assert_eq!(
        discover_bounded(&[], temp.path(), cap).unwrap_err(),
        DiscoveryError::FileByteLimit
    );
}

#[test]
fn bounded_stream_hash_matches_blake3_and_reads_only_one_over_budget_byte() {
    let bytes = vec![0xab; 20_000];
    let mut reader = Cursor::new(&bytes);
    let mut budget = bytes.len() as u64;
    assert_eq!(
        hash_reader(&mut reader, &mut budget).unwrap(),
        core_prov::content_hash(&bytes)
    );
    assert_eq!(budget, 0);
    let mut reader = Cursor::new(&bytes);
    let mut budget = 3;
    assert_eq!(
        hash_reader(&mut reader, &mut budget).unwrap_err(),
        DiscoveryError::FileByteLimit
    );
    assert_eq!(reader.position(), 4);
    let mut reader = Cursor::new(b"late-growth");
    assert_eq!(
        hash_reader(&mut reader, &mut 0).unwrap_err(),
        DiscoveryError::FileByteLimit
    );
    assert_eq!(reader.position(), 1);
}

#[test]
fn bounded_discovery_rejects_nonregular_wasm_entries() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("directory.wasm")).unwrap();
    assert_eq!(
        discover_bounded(&[], temp.path(), limits()).unwrap_err(),
        DiscoveryError::InvalidArtifact
    );
}

#[test]
#[cfg(unix)]
fn bounded_discovery_rejects_symlink_wasm_entries() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target");
    std::fs::write(&target, b"bytes").unwrap();
    std::os::unix::fs::symlink(&target, temp.path().join("linked.wasm")).unwrap();
    assert_eq!(
        discover_bounded(&[], temp.path(), limits()).unwrap_err(),
        DiscoveryError::InvalidArtifact
    );
}

#[test]
fn bounded_discovery_metadata_admits_retained_identity_copies_exactly() {
    let temp = tempfile::tempdir().unwrap();
    write(temp.path(), "adapter.wasm", b"artifact");
    let directory = temp.path().as_os_str().as_encoded_bytes().len();
    let name = "adapter.wasm".len();
    let stem = "adapter".len();
    let total = (directory + 1) + (name + 1) + 2 * (directory + name + 2) + 2 * (stem + 1) + 65;
    let mut cap = limits();
    cap.max_metadata_bytes = total;
    assert_eq!(discover_bounded(&[], temp.path(), cap).unwrap().len(), 1);
    cap.max_metadata_bytes -= 1;
    assert_eq!(
        discover_bounded(&[], temp.path(), cap).unwrap_err(),
        DiscoveryError::MetadataLimit
    );
}
