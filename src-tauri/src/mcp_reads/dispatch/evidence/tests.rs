//! #718 syntax-boundary tests; real source fixture is in primary_source_tests.
use super::*;

fn description() -> Value {
    json!({"mode":"describe","kind":"retained_primary_source","session_id":"session","basis_id":"basis","fact":{"kind":"node","id":"node"},"ordinal":0,"max_bytes":4096})
}
fn read() -> Value {
    json!({"mode":"read","kind":"retained_primary_source","session_id":"session","basis_id":"basis","max_bytes":4096,"target":{"schema_version":1,"target_id":"target","graph_snapshot_id":"graph","fact":{"kind":"node","id":"node"},"fact_digest":"digest","registered_source_id":"source","receipt_id":"receipt","receipt_inventory_index":0,"role":"provenance","index":0,"source":{"repo":"repo","path":"file.ts","byte_start":0,"byte_end":1,"commit_sha":"commit"},"captured":{"file":{"source_id":"source","capture_id":"capture","path":"file.ts","digest":"digest","byte_len":1},"byte_start":0,"byte_end":1}}})
}

#[test]
fn closed_evidence_modes_and_ref_kinds_reject_unknown_tags_and_fields() {
    assert!(serde_json::from_value::<EvidenceArgs>(description()).is_ok());
    assert!(serde_json::from_value::<EvidenceArgs>(read()).is_ok());
    for (field, value) in [
        ("mode", json!("execute")),
        ("kind", json!("working_tree_unverified")),
        ("path", json!("/private/secret")),
        ("max_bytes", json!(0)),
        ("ordinal", json!(-1)),
    ] {
        let mut args = description();
        args[field] = value;
        assert!(serde_json::from_value::<EvidenceArgs>(args).is_err());
    }
    let mut args = description();
    args["fact"]["sql"] = json!("secret");
    assert!(serde_json::from_value::<EvidenceArgs>(args).is_err());
    let mut args = read();
    args["fact"] = json!({"kind":"node","id":"wrong mode"});
    assert!(serde_json::from_value::<EvidenceArgs>(args).is_err());
}

#[test]
fn all_target_metadata_objects_reject_unadvertised_fields() {
    for location in ["target", "fact", "source", "captured", "file"] {
        let mut args = read();
        let object = match location {
            "target" => &mut args["target"],
            "fact" => &mut args["target"]["fact"],
            "source" => &mut args["target"]["source"],
            "captured" => &mut args["target"]["captured"],
            _ => &mut args["target"]["captured"]["file"],
        };
        object["command"] = json!("secret");
        assert!(serde_json::from_value::<EvidenceArgs>(args).is_err());
    }
    let schema = descriptor().1;
    assert_eq!(schema["oneOf"][0]["additionalProperties"], false);
    assert_eq!(
        schema["oneOf"][1]["properties"]["target"]["properties"]["source"]["additionalProperties"],
        false
    );
    assert_eq!(
        schema["oneOf"][0]["properties"]["kind"]["const"],
        "retained_primary_source"
    );
}
