use super::v3_tests::{captured_rule_receipt, sign_wire};
use super::*;
use crate::captured::{ClosureGapReason, ClosureInput, ClosureParameters, InputClosureV1};

fn established(v3: &Receipt) -> ReceiptClosure {
    let closure = InputClosureV1::new(
        "t0.adapter-ts/direct-lexical-v4",
        vec![GRAMMAR_PACKAGE.into(), PARSER_PACKAGE.into()],
        ClosureParameters {
            repo_key: v3.repo_key().into(),
            walk_policy: "walk-v1".into(),
            layer_hints: Vec::new(),
            grammar_rule: "grammar-by-path-v1".into(),
        },
        vec![ClosureInput::CapturedFile {
            file: v3.file().clone(),
        }],
    )
    .unwrap();
    ReceiptClosure::Established {
        closure_id: closure.closure_id,
    }
}

// AC-1080: a v4 receipt keeps v3's fact, ranges and compact form under its
// own contract, records its closure state and round-trips byte for byte.
#[test]
fn receipt_v4_records_its_closure_state_and_round_trips() {
    let (node, v3) = captured_rule_receipt();
    assert_eq!(v3.closure(), None);
    let v3_json = v3.to_json().unwrap();
    assert!(v3_json.contains("\"input_closure\":\"input_closure_not_established\""));

    let unbound = ReceiptClosure::InputClosureNotEstablished {
        reasons: vec![ClosureGapReason::CrossFileInputUnbound],
    };
    for closure in [established(&v3), unbound] {
        let v4 = v3.clone().with_closure(closure.clone()).unwrap();
        assert!(v4.id().starts_with("ts-primary-v4:"), "{}", v4.id());
        assert_ne!(v4.id(), v3.id());
        assert_eq!(v4.ranges(), v3.ranges());
        assert_eq!(v4.fact_digest(), v3.fact_digest());
        assert!(v4.matches_node(&node));
        assert_eq!(v4.closure(), Some(&closure));
        let json = v4.to_json().unwrap();
        assert!(json.contains("\"producer_contract\":\"t0.adapter-ts/direct-lexical-v4\""));
        let decoded = Receipt::from_json(&json).unwrap();
        assert_eq!(decoded, v4);
        assert_eq!(decoded.to_json().unwrap(), json);
        // Already v4: a published receipt is never retrofitted again.
        assert!(v4.with_closure(closure).is_err());
    }
    // v3 is unchanged by the new contract.
    assert_eq!(
        Receipt::from_json(&v3_json).unwrap().to_json().unwrap(),
        v3_json
    );
}

// AC-1080: a v4 receipt must carry a well-formed closure state and v1 to v3
// keep the legacy string; anything else is refused even when re-signed.
#[test]
fn receipt_v4_refuses_a_malformed_or_misplaced_closure_state() {
    let (_, v3) = captured_rule_receipt();
    let v4 = v3.clone().with_closure(established(&v3)).unwrap();
    let wire: serde_json::Value = serde_json::from_str(&v4.to_json().unwrap()).unwrap();
    let refuse = |closure: serde_json::Value| {
        let mut wire = wire.clone();
        wire["content"]["input_closure"] = closure.clone();
        assert!(
            Receipt::from_json(&sign_wire(wire, 4)).is_err(),
            "accepted {closure}"
        );
    };
    refuse(serde_json::json!("input_closure_not_established"));
    refuse(serde_json::json!({"state": "input_closure_not_established", "reasons": []}));
    refuse(serde_json::json!({
        "state": "input_closure_not_established",
        "reasons": ["remote_reference", "cross_file_input_unbound"]
    }));
    refuse(serde_json::json!({
        "state": "input_closure_not_established",
        "reasons": ["remote_reference", "remote_reference"]
    }));
    refuse(serde_json::json!({"state": "established", "closure_id": "closure-v1:nothex"}));
    refuse(
        serde_json::json!({"state": "established", "closure_id": format!("closure-v1:{}", "a".repeat(64)), "extra": 1}),
    );
    refuse(serde_json::json!({"state": "unknown"}));

    let mut legacy: serde_json::Value = serde_json::from_str(&v3.to_json().unwrap()).unwrap();
    legacy["content"]["input_closure"] = wire["content"]["input_closure"].clone();
    assert!(Receipt::from_json(&sign_wire(legacy, 3)).is_err());
}
