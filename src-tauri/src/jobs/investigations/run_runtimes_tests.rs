use super::*;
use crate::jobs::investigations::{InvestigationDetailRead, RunRuntime};

fn pi(version: &str) -> RunRuntime {
    RunRuntime {
        lane: "pi".into(),
        runtime: "cartograph-agent".into(),
        version: version.into(),
    }
}

// AC-1423: a run on the tool-calling runtime names the harness, runtime and
// version beside the saved provider id and model; the first record stands
// and reopen reads it back unchanged. A bounded-loop run names none.
#[test]
fn a_tool_calling_run_names_its_runtime_and_reopen_keeps_it() {
    let mut fixture = Fixture::new();
    let (id, _execution) = fixture.start("runtime-record", InvestigationProviderMode::Local);
    let (other, _other_execution) = fixture.start("runtime-none", InvestigationProviderMode::Local);
    assert_eq!(fixture.store.investigation_runtime(&id).unwrap(), None);

    fixture
        .store
        .record_investigation_runtime(&id, &pi("0.1.0"))
        .unwrap();
    // A restarted runtime reporting the same identity is not a new record.
    fixture
        .store
        .record_investigation_runtime(&id, &pi("0.1.0"))
        .unwrap();
    assert!(matches!(
        fixture
            .store
            .record_investigation_runtime(&id, &pi("0.2.0")),
        Err(InvestigationStoreError::Invalid)
    ));
    for invalid in [
        RunRuntime {
            lane: "codex".into(),
            ..pi("0.1.0")
        },
        pi(""),
        pi("0.1.0 beta"),
        pi(&"9".repeat(65)),
    ] {
        assert!(matches!(
            fixture.store.record_investigation_runtime(&other, &invalid),
            Err(InvestigationStoreError::Invalid)
        ));
    }
    assert!(matches!(
        fixture
            .store
            .record_investigation_runtime("inv_missing", &pi("0.1.0")),
        Err(InvestigationStoreError::Missing | InvestigationStoreError::Invalid)
    ));

    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(
        reopened.investigation_runtime(&id).unwrap(),
        Some(pi("0.1.0"))
    );
    assert_eq!(reopened.investigation_runtime(&other).unwrap(), None);

    let detail = reopened.investigation(&id).unwrap();
    let read = InvestigationDetailRead::from_detail(detail.clone())
        .unwrap()
        .with_runtime(reopened.investigation_runtime(&id).unwrap());
    let json = serde_json::to_value(&read).unwrap();
    assert_eq!(
        json["runtime"],
        serde_json::json!({"lane": "pi", "runtime": "cartograph-agent", "version": "0.1.0"})
    );
    assert_eq!(json["provider"]["provider_id"], detail.provider.provider_id);
    assert_eq!(json["provider"]["model"], detail.provider.model);
    let plain =
        serde_json::to_value(InvestigationDetailRead::from_detail(detail).unwrap()).unwrap();
    assert!(plain.get("runtime").is_none());

    // The saved row is immutable.
    let tamper = reopened
        .conn
        .execute("UPDATE run_runtime_records SET version='9'", []);
    assert!(tamper.is_err());
}
