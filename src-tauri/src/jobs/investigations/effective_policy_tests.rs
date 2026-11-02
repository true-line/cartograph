//! Saved-policy authority is determined by the recorded host origin.
use super::*;

#[test]
fn saved_desktop_time_override_preserves_limits_and_mcp_stays_static() {
    let mut fixture = Fixture::new();
    let (id, _execution) = fixture.start("effective-policy", InvestigationProviderMode::Local);
    let mut detail = fixture.store.investigation(&id).unwrap();
    let original = detail.limits.clone();
    assert_eq!(policy_for_detail(&detail).unwrap().limits(), original);

    for seconds in [600, 601, 3600] {
        detail.limits.active_seconds = seconds;
        detail.origin = InvestigationOrigin::App;
        let effective = policy_for_detail(&detail).unwrap();
        assert_eq!(effective.limits(), detail.limits);
        assert_eq!(effective.limits().wall_seconds, original.wall_seconds);
        // Round-tripping the saved record retains the task-time choice.
        let reopened =
            serde_json::from_slice::<InvestigationDetail>(&serde_json::to_vec(&detail).unwrap())
                .unwrap();
        assert_eq!(
            policy_for_detail(&reopened).unwrap().limits(),
            detail.limits
        );
        detail.origin = InvestigationOrigin::Mcp {
            client_id: McpClientId::from_host_bytes([7; 16]),
        };
        assert_eq!(
            policy_for_detail(&detail).is_ok(),
            seconds == original.active_seconds
        );
    }

    detail.origin = InvestigationOrigin::App;
    for seconds in [0, 599, 3601, u64::MAX] {
        detail.limits.active_seconds = seconds;
        assert!(policy_for_detail(&detail).is_err());
    }
    detail.limits = original;
    detail.limits.active_seconds = 3600;
    let serialized = serde_json::to_value(&detail.limits).unwrap();
    for field in serialized.as_object().unwrap().keys() {
        if field == "active_seconds" {
            continue;
        }
        let mut changed = serialized.clone();
        let value = &mut changed[field];
        if let Some(number) = value.as_u64() {
            *value = serde_json::json!(number + 1);
        } else {
            *value = serde_json::json!("unknown-profile");
        }
        detail.limits = serde_json::from_value(changed).unwrap();
        assert!(
            policy_for_detail(&detail).is_err(),
            "accepted changed {field}"
        );
    }
}
