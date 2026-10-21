//! Extension event registries as the app loads them (#594, AC-0255).
use crate::{Registries, coverage_config_hash, event_registries_in};
use adapters_fw::events::{MAX_EXTENSION_FILES, MAX_REGISTRY_BYTES};

const NATS: &str = r#"{"schema":"cartograph-event-sdk-registry@1","version":"nats-1",
  "sdks":[{"kind":"nats-subject","role":"produces",
    "pattern":{"method":{"module":"nats","ctor":"Client","method":"publish"}},
    "identity":"first"}]}"#;

#[test]
fn extension_registries_load_bounded_and_bind_the_coverage_identity() {
    let dir = tempfile::tempdir().unwrap();
    let missing = event_registries_in(&dir.path().join("absent"));
    assert_eq!(missing.identity(), Registries::builtin().identity());
    assert!(missing.rejected.is_empty());

    std::fs::write(dir.path().join("nats.json"), NATS).unwrap();
    std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
    std::fs::write(
        dir.path().join("oversized.json"),
        vec![b' '; MAX_REGISTRY_BYTES + 4096],
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("nested.json")).unwrap();
    let loaded = event_registries_in(dir.path());
    let names: Vec<&str> = loaded.sources.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "builtin:event-sdks.json",
            "builtin:channel-backings.json",
            "extension:nats.json"
        ]
    );
    let rejected: Vec<&str> = loaded.rejected.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        rejected,
        ["extension:nested.json", "extension:oversized.json"]
    );
    assert!(loaded.rejected[1].reason.contains("larger than"));

    // The coverage basis moves with the accepted set and nothing else.
    let builtin = coverage_config_hash(&[], Registries::builtin());
    let extended = coverage_config_hash(&[], &loaded);
    assert_ne!(builtin, extended);
    std::fs::write(dir.path().join("oversized.json"), "{").unwrap();
    assert_eq!(
        coverage_config_hash(&[], &event_registries_in(dir.path())),
        extended
    );

    // Files past the count bound are rejected unread.
    for i in 0..MAX_EXTENSION_FILES {
        std::fs::write(dir.path().join(format!("x{i:02}.json")), "{").unwrap();
    }
    let crowded = event_registries_in(dir.path());
    assert!(
        crowded
            .rejected
            .iter()
            .any(|r| r.reason.contains("extension files"))
    );
    assert_eq!(crowded.rejected.len(), MAX_EXTENSION_FILES + 2);
}
