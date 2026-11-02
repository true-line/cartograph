//! Acceptance evidence only: never a provider authorization or an execution timer.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};

const LIMIT: usize = 128 * 1024;
const FIELD: &str = "start_latency_sha256";
const BOUNDARY: &str = "production_start_call_to_returned_task_id";

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Timing {
    schema: String,
    role: String,
    start_binding_sha256: String,
    request_sha256: String,
    boundary: String,
    elapsed_ns: u64,
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn wire(value: &impl Serialize) -> Result<Vec<u8>, String> {
    crate::investigations::context::bounded_json(value, LIMIT).map_err(|e| e.to_string())
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    super::super::vendure::read_preparation_artifact(path, LIMIT)
}
fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    super::analyst::exclusive(path, bytes)?;
    super::vendure_prepare::sync_directory(path.parent().ok_or("Missing start directory")?)
}

/// `elapsed` exists only for a newly created task. Old start bytes remain valid
/// with unknown timing only under their independently supplied exact start pin.
/// The expected pin must never be read from the artifacts being checked.
/// The start binding contains the definition hash and the returned task ID.
pub(super) fn seal_or_validate(
    directory: &Path,
    start: &Value,
    role: &str,
    request_sha256: &str,
    elapsed: Option<Duration>,
    expected_start_sha256: Option<&str>,
) -> Result<Option<Timing>, String> {
    if !matches!(role, "analyst" | "auditor")
        || request_sha256.len() != 64
        || !request_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || !start.is_object()
        || start.get(FIELD).is_some()
    {
        return Err("Invalid start timing binding".into());
    }
    let binding = hash(&wire(start)?);
    let timing_path = directory.join("start-latency.json");
    let start_path = directory.join("start.json");
    if let Some(elapsed) = elapsed {
        if expected_start_sha256.is_some() {
            return Err("Fresh start cannot reuse a prior start pin".into());
        }
        let timing = Timing {
            schema: "cartograph-captured-start-latency@1".into(),
            role: role.into(),
            start_binding_sha256: binding,
            request_sha256: request_sha256.into(),
            boundary: BOUNDARY.into(),
            elapsed_ns: elapsed
                .as_nanos()
                .try_into()
                .map_err(|_| "Start duration overflow")?,
        };
        let bytes = wire(&timing)?;
        // Sidecar first: a crash before the start seal remains an explicit
        // recovery case, never permission to create another task or retime it.
        write(&timing_path, &bytes)?;
        let mut sealed = start.clone();
        sealed[FIELD] = Value::String(hash(&bytes));
        let start_bytes = wire(&sealed)?;
        write(&start_path, &start_bytes)?;
        // Retain this staging output independently before any later reopen.
        eprintln!("captured-{role}-start-sha256={}", hash(&start_bytes));
        return Ok(Some(timing));
    }
    let expected = expected_start_sha256.ok_or("Independent start SHA256 required for reopen")?;
    let bytes = read(&start_path)?;
    if expected != hash(&bytes) {
        return Err("Pinned start bytes changed".into());
    }
    let mut sealed: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let digest = sealed
        .as_object_mut()
        .ok_or("Invalid start seal")?
        .remove(FIELD);
    if &sealed != start || wire(&sealed)? != wire(start)? {
        return Err("Start identity changed".into());
    }
    let Some(digest) = digest else {
        if timing_path.try_exists().map_err(|e| e.to_string())? {
            return Err("Unsealed start timing; explicit recovery required".into());
        }
        // Preserve the historical exact-wire check, including malformed data.
        if bytes != wire(start)? {
            return Err("Historical start bytes changed".into());
        }
        return Ok(None);
    };
    let timing_bytes = read(&timing_path)?;
    if digest.as_str() != Some(hash(&timing_bytes).as_str()) {
        return Err("Start timing digest changed".into());
    }
    let timing: Timing = serde_json::from_slice(&timing_bytes).map_err(|e| e.to_string())?;
    if timing.schema != "cartograph-captured-start-latency@1"
        || timing.role != role
        || timing.start_binding_sha256 != binding
        || timing.request_sha256 != request_sha256
        || timing.boundary != BOUNDARY
    {
        return Err("Start timing binding changed".into());
    }
    let mut expected = start.clone();
    expected[FIELD] = digest;
    if bytes != wire(&expected)? {
        return Err("Start seal bytes changed".into());
    }
    Ok(Some(timing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // AC-0463: zero is a valid measured duration; reopen returns the original
    // value, does not write, and cannot replace the identity or request binding.
    #[test]
    fn captured_start_latency_seals_and_reuses_original_measurement() {
        for role in ["analyst", "auditor"] {
            let dir = tempfile::tempdir().unwrap();
            let start = json!({"definition_sha256":"a".repeat(64),"task_id":"task1"});
            let request = "b".repeat(64);
            let first = seal_or_validate(
                dir.path(),
                &start,
                role,
                &request,
                Some(Duration::ZERO),
                None,
            )
            .unwrap();
            let before = read(&dir.path().join("start.json")).unwrap();
            let pin = hash(&before);
            assert_eq!(
                first,
                seal_or_validate(dir.path(), &start, role, &request, None, Some(&pin)).unwrap()
            );
            assert_eq!(before, read(&dir.path().join("start.json")).unwrap());
            assert_eq!(first.unwrap().elapsed_ns, 0);
            for changed in [
                json!({"definition_sha256":"c".repeat(64),"task_id":"task1"}),
                json!({"definition_sha256":"a".repeat(64),"task_id":"task2"}),
            ] {
                assert!(
                    seal_or_validate(dir.path(), &changed, role, &request, None, Some(&pin))
                        .is_err()
                );
            }
            assert!(
                seal_or_validate(dir.path(), &start, role, &"c".repeat(64), None, Some(&pin))
                    .is_err()
            );
            assert!(
                seal_or_validate(
                    dir.path(),
                    &start,
                    role,
                    &request,
                    Some(Duration::from_secs(1)),
                    None
                )
                .is_err()
            );
            let sidecar = dir.path().join("start-latency.json");
            let original = read(&sidecar).unwrap();
            let mut tampered: Value = serde_json::from_slice(&original).unwrap();
            tampered["elapsed_ns"] = json!(123);
            std::fs::write(&sidecar, wire(&tampered).unwrap()).unwrap();
            assert!(
                seal_or_validate(dir.path(), &start, role, &request, None, Some(&pin)).is_err()
            );
            std::fs::remove_file(&sidecar).unwrap();
            assert!(
                seal_or_validate(dir.path(), &start, role, &request, None, Some(&pin)).is_err()
            );
        }
    }

    // AC-0463: neither a self-consistent rewrite nor removal can replace
    // independently retained staging authority for either role.
    #[test]
    fn captured_start_latency_rejects_paired_rewrite_and_removal() {
        for role in ["analyst", "auditor"] {
            let dir = tempfile::tempdir().unwrap();
            let start = json!({"definition_sha256":"a".repeat(64),"task_id":"task1"});
            let request = "b".repeat(64);
            seal_or_validate(
                dir.path(),
                &start,
                role,
                &request,
                Some(Duration::ZERO),
                None,
            )
            .unwrap();
            let start_path = dir.path().join("start.json");
            let sidecar = dir.path().join("start-latency.json");
            let pin = hash(&read(&start_path).unwrap());
            assert!(seal_or_validate(dir.path(), &start, role, &request, None, None).is_err());
            let mut timing: Value = serde_json::from_slice(&read(&sidecar).unwrap()).unwrap();
            timing["elapsed_ns"] = json!(123);
            let changed = wire(&timing).unwrap();
            std::fs::write(&sidecar, &changed).unwrap();
            let mut sealed = start.clone();
            sealed[FIELD] = json!(hash(&changed));
            std::fs::write(&start_path, wire(&sealed).unwrap()).unwrap();
            assert_eq!(
                seal_or_validate(dir.path(), &start, role, &request, None, Some(&pin)).unwrap_err(),
                "Pinned start bytes changed"
            );
            std::fs::remove_file(&sidecar).unwrap();
            std::fs::write(&start_path, wire(&start).unwrap()).unwrap();
            assert_eq!(
                seal_or_validate(dir.path(), &start, role, &request, None, Some(&pin)).unwrap_err(),
                "Pinned start bytes changed"
            );
            assert!(seal_or_validate(dir.path(), &start, role, &request, None, None).is_err());
        }
    }

    // AC-0463: legacy unknown is not a new measurement. Incomplete new seals
    // require recovery, and invalid/overflowing timing never creates evidence.
    #[test]
    fn captured_start_latency_preserves_unknown_and_rejects_malformed_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let start = json!({"definition_sha256":"a".repeat(64),"task_id":"task1"});
        let request = "b".repeat(64);
        assert!(
            seal_or_validate(
                dir.path(),
                &start,
                "analyst",
                &request,
                Some(Duration::MAX),
                None
            )
            .is_err()
        );
        assert!(!dir.path().join("start-latency.json").exists());
        write(&dir.path().join("start.json"), &wire(&start).unwrap()).unwrap();
        let pin = hash(&wire(&start).unwrap());
        assert!(
            seal_or_validate(dir.path(), &start, "analyst", &request, None, Some(&pin))
                .unwrap()
                .is_none()
        );
        let malformed = br#"{"elapsed_ns":-1}"#;
        std::fs::write(dir.path().join("start-latency.json"), malformed).unwrap();
        assert!(
            seal_or_validate(dir.path(), &start, "analyst", &request, None, Some(&pin)).is_err()
        );
        let mut sealed = start.clone();
        sealed[FIELD] = json!(hash(malformed));
        std::fs::write(dir.path().join("start.json"), wire(&sealed).unwrap()).unwrap();
        assert!(
            seal_or_validate(dir.path(), &start, "analyst", &request, None, Some(&pin)).is_err()
        );
    }
}
