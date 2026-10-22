use super::*;
use serde_json::json;

pub(crate) fn limits() -> ScopeLimits {
    ScopeLimits {
        max_sources: NonZeroUsize::new(32).unwrap(),
        max_source_registry_bytes: NonZeroUsize::new(1_000_000).unwrap(),
        max_event_registry_bytes: NonZeroUsize::new(1_000_000).unwrap(),
        max_directory_entries: NonZeroUsize::new(128).unwrap(),
        max_artifact_bytes: NonZeroU64::new(1_000_000).unwrap(),
        max_metadata_bytes: NonZeroUsize::new(1_000_000).unwrap(),
        max_corpus_bytes: NonZeroU64::new(1_000_000).unwrap(),
    }
}
fn dir(parent: &Path, name: &str) -> std::path::PathBuf {
    let path = parent.join(name);
    std::fs::create_dir_all(&path).unwrap();
    crate::paths::canonicalize(path).unwrap()
}
const SDK: &str = r#"{"schema":"cartograph-event-sdk-registry@1","version":"fixture-1","sdks":[{"kind":"nats-subject","role":"produces","pattern":{"method":{"module":"nats","ctor":"Client","method":"publish"}},"identity":"first"}]}"#;
fn plugin(state: &crate::AppState, root: &Path, name: &str) -> Vec<u8> {
    let adapters = dir(root, adapters_plugin_host::discovery::PROJECT_ADAPTER_DIR);
    let artifact = format!("fixture-{name}").into_bytes();
    let corpus = json!({"extensions":[name],"cases":[{"path":"fixture","source":"fixture"}]})
        .to_string()
        .into_bytes();
    std::fs::write(adapters.join(format!("{name}.wasm")), &artifact).unwrap();
    std::fs::write(adapters.join(format!("{name}.golden.json")), &corpus).unwrap();
    let hash = core_prov::content_hash(&artifact);
    let gate = adapters_plugin_host::gate::gate_key(&hash, Some(&corpus));
    let mut settings = state.settings.lock().unwrap();
    settings
        .set_plugin_enabled(&root.display().to_string(), name, &hash, true)
        .unwrap();
    // Synthetic persisted verdict only: no plugin, provider or gate execution.
    settings
        .record_plugin_gate(name, &gate, true, "{}")
        .unwrap();
    corpus
}

#[test]
fn mcp_bounded_scope_matches_normal_plugin_and_layered_registry_basis() {
    // AC-0288: MCP bounded acquisition preserves official coverage config bytes.
    let temp = tempfile::tempdir().unwrap();
    let private = dir(temp.path(), "private");
    let root = dir(temp.path(), "project");
    let user = dir(temp.path(), "user");
    let events = dir(temp.path(), "events");
    let state = crate::registered_source_tests::app_state(&private);
    let source = crate::register_local_source(&state, &root).unwrap();
    plugin(&state, &root, "fixture");
    std::fs::write(events.join("sdk.json"), SDK).unwrap();
    std::fs::write(events.join("invalid.json"), "invalid registry").unwrap();
    let actual = coverage_scope_in(&state, &user, &events, limits()).unwrap();
    let official_registries = crate::event_registries_in(&events);
    let bounded_registries = event_registries(&events, limits()).unwrap();
    assert_eq!(
        serde_json::to_value(&bounded_registries.sources).unwrap(),
        serde_json::to_value(&official_registries.sources).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&bounded_registries.rejected).unwrap(),
        serde_json::to_value(&official_registries.rejected).unwrap()
    );
    let official_plugins = crate::active_plugins_in(&state, &root, &user).unwrap();
    assert_eq!(official_plugins.len(), 1);
    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0].repo_key, source.repo_key);
    assert_eq!(
        actual[0].config_hash,
        crate::coverage_config_hash(&official_plugins, &official_registries)
    );
    // The legacy wrapper remains callable and returns the same normal basis.
    assert_eq!(
        official_plugins[0].gate_key,
        active_plugins(&state, &root, &user, limits()).unwrap()[0].gate_key
    );
}

#[test]
fn mcp_scope_rejects_source_rows_and_bytes_before_filesystem_scan() {
    let temp = tempfile::tempdir().unwrap();
    let private = dir(temp.path(), "private");
    let state = crate::registered_source_tests::app_state(&private);
    for name in ["one", "two"] {
        crate::register_local_source(&state, &dir(temp.path(), name)).unwrap();
    }
    let invalid_dir = temp.path().join("not-a-directory");
    std::fs::write(&invalid_dir, "fixture").unwrap();
    let mut cap = limits();
    cap.max_sources = NonZeroUsize::new(1).unwrap();
    assert_eq!(
        coverage_scope_in(&state, &invalid_dir, &invalid_dir, cap).unwrap_err(),
        ReadError::Limit
    );
    cap = limits();
    cap.max_source_registry_bytes = NonZeroUsize::new(1).unwrap();
    assert_eq!(
        coverage_scope_in(&state, &invalid_dir, &invalid_dir, cap).unwrap_err(),
        ReadError::Limit
    );
    assert_eq!(
        coverage_scope_in(&state, &invalid_dir, &invalid_dir, limits()).unwrap_err(),
        ReadError::Storage
    );
}

#[test]
fn mcp_scope_enforces_plugin_artifact_corpus_directory_and_metadata_limits() {
    let temp = tempfile::tempdir().unwrap();
    let private = dir(temp.path(), "private");
    let root = dir(temp.path(), "project");
    let user = dir(temp.path(), "user");
    let state = crate::registered_source_tests::app_state(&private);
    crate::register_local_source(&state, &root).unwrap();
    let corpus = plugin(&state, &root, "fixture");
    for field in ["artifact", "corpus", "entries", "metadata"] {
        let mut cap = limits();
        match field {
            "artifact" => cap.max_artifact_bytes = NonZeroU64::new(1).unwrap(),
            "corpus" => cap.max_corpus_bytes = NonZeroU64::new(corpus.len() as u64 - 1).unwrap(),
            "entries" => cap.max_directory_entries = NonZeroUsize::new(1).unwrap(),
            "metadata" => cap.max_metadata_bytes = NonZeroUsize::new(1).unwrap(),
            _ => unreachable!(),
        }
        assert_eq!(
            active_plugins(&state, &root, &user, cap).err(),
            Some(ReadError::Limit),
            "{field}"
        );
    }
    let mut cap = limits();
    cap.max_corpus_bytes = NonZeroU64::new(corpus.len() as u64).unwrap();
    assert_eq!(active_plugins(&state, &root, &user, cap).unwrap().len(), 1);
    plugin(&state, &root, "second");
    assert_eq!(
        active_plugins(&state, &root, &user, cap).err(),
        Some(ReadError::Limit)
    );
}

#[test]
fn mcp_scope_bounds_all_event_entries_names_and_file_bytes_before_layering() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("sdk.json"), SDK).unwrap();
    std::fs::write(temp.path().join("ignored.txt"), "ignored").unwrap();
    let mut cap = limits();
    cap.max_directory_entries = NonZeroUsize::new(1).unwrap();
    assert_eq!(
        event_registries(temp.path(), cap).err(),
        Some(ReadError::Limit)
    );
    cap = limits();
    cap.max_event_registry_bytes = NonZeroUsize::new(SDK.len() - 1).unwrap();
    assert_eq!(
        event_registries(temp.path(), cap).err(),
        Some(ReadError::Limit)
    );
    cap.max_event_registry_bytes = NonZeroUsize::new(SDK.len()).unwrap();
    assert_eq!(
        event_registries(temp.path(), cap).unwrap().identity(),
        crate::event_registries_in(temp.path()).identity()
    );
    cap = limits();
    cap.max_metadata_bytes = NonZeroUsize::new(1).unwrap();
    assert_eq!(
        event_registries(temp.path(), cap).err(),
        Some(ReadError::Limit)
    );
}

#[test]
fn mcp_scope_corpus_limit_precedes_json_parse_and_allows_exact_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("corpus.json");
    std::fs::write(&path, [0xff_u8; 8192]).unwrap();
    assert_eq!(
        read_file(&path, &mut 3, None).unwrap_err(),
        ReadError::Limit
    );
    assert_eq!(
        read_file(&path, &mut 8192, None).unwrap().unwrap().len(),
        8192
    );
    let missing = temp.path().join("absent.json");
    assert!(read_file(&missing, &mut 1, None).unwrap().is_none());
}

#[test]
fn mcp_scope_preserves_official_oversize_and_file_count_rejection_identity() {
    use adapters_fw::events::{MAX_EXTENSION_FILES, MAX_REGISTRY_BYTES};
    let temp = tempfile::tempdir().unwrap();
    let oversized = temp.path().join("oversized.json");
    std::fs::write(&oversized, vec![b'x'; MAX_REGISTRY_BYTES + 100]).unwrap();
    let mut cap = limits();
    cap.max_event_registry_bytes = NonZeroUsize::new(MAX_REGISTRY_BYTES + 1).unwrap();
    let bounded = event_registries(temp.path(), cap).unwrap();
    let official = crate::event_registries_in(temp.path());
    assert_eq!(bounded.identity(), official.identity());
    assert_eq!(
        serde_json::to_value(bounded.rejected).unwrap(),
        serde_json::to_value(official.rejected).unwrap()
    );
    std::fs::remove_file(oversized).unwrap();
    for index in 0..MAX_EXTENSION_FILES + 1 {
        std::fs::write(temp.path().join(format!("{index:03}.json")), "{}").unwrap();
    }
    let bounded = event_registries(temp.path(), limits()).unwrap();
    let official = crate::event_registries_in(temp.path());
    assert_eq!(bounded.identity(), official.identity());
    assert_eq!(
        serde_json::to_value(bounded.rejected).unwrap(),
        serde_json::to_value(official.rejected).unwrap()
    );
}
