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

// AC-0570: only the pinned hosted Opus lane takes the output allowance; a
// saved desktop task recovers it exactly, and MCP stays on static policy.
#[test]
fn hosted_opus_output_allowance_is_lane_scoped_and_recovers_exactly() {
    let provider = |mode, provider_id: &str| InvestigationProvider {
        mode,
        provider_id: provider_id.into(),
        model: "claude-opus-5-5".into(),
        endpoint: "https://api.anthropic.com/v1/messages".into(),
        deployment: None,
        protocol_version: None,
        available: true,
        unavailable_reason: None,
    };
    let cloud = InvestigationProviderMode::Cloud;
    assert!(hosted_generation(Some(&provider(
        cloud,
        "anthropic:claude-opus-5-5"
    ))));
    for (mode, id) in [
        (
            InvestigationProviderMode::Local,
            "anthropic:claude-opus-5-5",
        ),
        (cloud, "anthropic:claude-haiku-4-5-20251001"),
        (cloud, "anthropic:claude-fable-5"),
        (cloud, "openai:gpt-6.1-sol"),
    ] {
        assert!(!hosted_generation(Some(&provider(mode, id))), "{id}");
    }
    assert!(!hosted_generation(None));

    let mut fixture = Fixture::new();
    let (id, _execution) = fixture.start("hosted-allowance", InvestigationProviderMode::Local);
    let mut detail = fixture.store.investigation(&id).unwrap();
    let hosted = InvestigationPolicy::for_profile("investigation-v2")
        .unwrap()
        .with_active_seconds(1200)
        .unwrap()
        .with_hosted_generation()
        .unwrap();
    detail.limits = hosted.limits();
    detail.origin = InvestigationOrigin::App;
    // The exact hosted pair alone never authorizes it: a Local, non-Opus or
    // non-next-prompt saved task carrying it is rejected on recovery.
    assert!(policy_for_detail(&detail).is_err());
    detail.summary.specialist_id = SpecialistId::DomainAnalystV12;
    assert!(policy_for_detail(&detail).is_err());
    detail.provider = provider(cloud, "anthropic:claude-haiku-4-5-20251001");
    detail.summary.provider_mode = cloud;
    assert!(policy_for_detail(&detail).is_err());
    detail.provider = provider(cloud, "anthropic:claude-opus-5-5");
    detail.summary.specialist_id = SpecialistId::DomainAnalyst;
    assert!(policy_for_detail(&detail).is_err());
    detail.summary.specialist_id = SpecialistId::DomainAnalystV12;
    detail.summary.provider_mode = InvestigationProviderMode::Local;
    assert!(policy_for_detail(&detail).is_err());
    detail.summary.provider_mode = cloud;
    assert_eq!(policy_for_detail(&detail).unwrap().limits(), detail.limits);
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
    assert!(policy_for_detail(&detail).is_err());
}
