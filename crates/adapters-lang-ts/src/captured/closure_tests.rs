use super::*;

fn source() -> SourceId {
    SourceId::new("src_00000000000000000000000000000001").unwrap()
}

fn file(path: &str, digest: &str) -> CaptureFileRef {
    CaptureFileRef {
        source_id: source(),
        capture_id: "capture-v1:fixture".into(),
        path: path.into(),
        digest: digest.into(),
        byte_len: 12,
    }
}

fn parameters() -> ClosureParameters {
    ClosureParameters {
        repo_key: "repo".into(),
        walk_policy: "walk-v1".into(),
        layer_hints: vec!["web".into(), "api".into(), "web".into()],
        grammar_rule: "grammar-by-path-v1".into(),
    }
}

fn packages() -> Vec<String> {
    vec![
        "tree-sitter@0.27.2".into(),
        "tree-sitter-typescript@0.23.2".into(),
    ]
}

fn closure(inputs: Vec<ClosureInput>) -> InputClosureV1 {
    InputClosureV1::new(
        "t0.adapter-ts/direct-lexical-v4",
        packages(),
        parameters(),
        inputs,
    )
    .unwrap()
}

fn captured(path: &str, digest: &str) -> ClosureInput {
    ClosureInput::CapturedFile {
        file: file(path, digest),
    }
}

fn upstream(id: &str, closure: ClosureRef) -> ClosureInput {
    ClosureInput::UpstreamFact {
        fact: FactKey::Node { id: id.into() },
        fact_digest: "digest".into(),
        closure,
    }
}

// AC-1078: the id is BLAKE3 over the domain and the canonical record without
// its own field; it is stable, and input order and repeats do not change it.
#[test]
fn the_closure_id_is_canonical_and_excludes_itself() {
    let one = closure(vec![captured("b.ts", "d2"), captured("a.ts", "d1")]);
    let two = closure(vec![
        captured("a.ts", "d1"),
        captured("b.ts", "d2"),
        captured("a.ts", "d1"),
    ]);
    assert_eq!(one, two);
    assert!(is_closure_id(&one.closure_id), "{}", one.closure_id);
    assert_eq!(one.parameters.layer_hints, ["api", "web"]);

    let mut value = serde_json::to_value(&one).unwrap();
    value.as_object_mut().unwrap().remove("closure_id");
    let mut preimage = b"cartograph:input-closure:v1\0".to_vec();
    preimage.extend(serde_json::to_vec(&canonical(value)).unwrap());
    assert_eq!(
        one.closure_id,
        format!("closure-v1:{}", core_prov::content_hash(&preimage))
    );

    let bytes = one.canonical_bytes().unwrap();
    assert!(!bytes.contains(&b' '));
    assert_eq!(InputClosureV1::decode(&bytes).unwrap(), one);
}

// AC-1078: every bound input and parameter is part of the identity.
#[test]
fn changing_any_input_or_parameter_changes_the_closure_id() {
    let base = closure(vec![captured("a.ts", "d1")]);
    let changed_bytes = closure(vec![captured("a.ts", "d9")]);
    let extra_input = closure(vec![captured("a.ts", "d1"), captured("b.ts", "d2")]);
    let mut other = parameters();
    other.repo_key = "other".into();
    let other_parameters = InputClosureV1::new(
        "t0.adapter-ts/direct-lexical-v4",
        packages(),
        other,
        vec![captured("a.ts", "d1")],
    )
    .unwrap();
    let other_contract = InputClosureV1::new(
        "t0.adapter-ts/direct-lexical-v5",
        packages(),
        parameters(),
        vec![captured("a.ts", "d1")],
    )
    .unwrap();
    let ids = [
        &base,
        &changed_bytes,
        &extra_input,
        &other_parameters,
        &other_contract,
    ]
    .map(|closure| closure.closure_id.clone());
    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "{ids:?}");
}

// AC-1078: a stored record with a wrong id, unknown field, unsorted or
// repeated inputs, or no inputs is rejected.
#[test]
fn the_decoder_rejects_every_second_encoding() {
    let good = closure(vec![captured("a.ts", "d1"), captured("b.ts", "d2")]);
    let value = serde_json::to_value(&good).unwrap();
    let reject = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut value = value.clone();
        edit(&mut value);
        InputClosureV1::decode(&serde_json::to_vec(&value).unwrap()).unwrap_err()
    };
    reject(&|value| value["closure_id"] = format!("closure-v1:{}", "0".repeat(64)).into());
    reject(&|value| value["extra"] = 1.into());
    reject(&|value| value["schema_version"] = 2.into());
    reject(&|value| value["inputs"].as_array_mut().unwrap().reverse());
    reject(&|value| {
        let first = value["inputs"][0].clone();
        value["inputs"].as_array_mut().unwrap().push(first);
    });
    reject(&|value| value["inputs"] = serde_json::json!([]));
    reject(&|value| value["producer_packages"] = serde_json::json!([]));
    reject(&|value| value["inputs"][0]["file"]["mtime"] = 1.into());
}

// AC-1078: absence is relative to an enumeration the closure binds.
#[test]
fn an_absent_path_needs_its_enumeration() {
    let absent = ClosureInput::AbsentFromEnumeration {
        source_id: source(),
        capture_id: "capture-v1:fixture".into(),
        path: "tsconfig.json".into(),
    };
    let alone = InputClosureV1::new(
        "t0.adapter-ts/directory-v1",
        packages(),
        parameters(),
        vec![captured("a.ts", "d1"), absent.clone()],
    );
    assert!(alone.is_err());
    let bound = closure(vec![
        absent,
        ClosureInput::Enumeration {
            source_id: source(),
            capture_id: "capture-v1:fixture".into(),
            walk_policy: "walk-v1".into(),
        },
    ]);
    assert!(bound.gaps().is_empty());
}

// AC-1078: an upstream fact without an established closure keeps this one
// from being established; a fact is never its own upstream.
#[test]
fn an_incomplete_upstream_is_a_gap_and_a_fact_is_never_its_own_upstream() {
    let established = closure(vec![captured("a.ts", "d1")]);
    let complete = closure(vec![upstream(
        "node:a",
        ClosureRef::Established {
            closure_id: established.closure_id.clone(),
        },
    )]);
    assert!(complete.gaps().is_empty());
    let incomplete = closure(vec![upstream("node:a", ClosureRef::NotEstablished)]);
    assert_eq!(
        incomplete.gaps(),
        [ClosureGapReason::UpstreamClosureIncomplete]
    );
    assert!(
        incomplete
            .validate_for(&FactKey::Node {
                id: "node:a".into()
            })
            .is_err()
    );
    assert!(
        incomplete
            .validate_for(&FactKey::Node {
                id: "node:b".into()
            })
            .is_ok()
    );
    let malformed = InputClosureV1::new(
        "t0.adapter-ts/direct-lexical-v4",
        packages(),
        parameters(),
        vec![upstream(
            "node:a",
            ClosureRef::Established {
                closure_id: "closure-v1:nothex".into(),
            },
        )],
    );
    assert!(malformed.is_err());
}

// AC-1078: a closure over more inputs than the bound is refused, never cut.
#[test]
fn a_closure_over_the_input_bound_is_refused() {
    let inputs = (0..=MAX_CLOSURE_INPUTS)
        .map(|index| captured(&format!("f{index}.ts"), "d"))
        .collect();
    let error = InputClosureV1::new(
        "t0.adapter-ts/direct-lexical-v4",
        packages(),
        parameters(),
        inputs,
    )
    .unwrap_err();
    assert!(matches!(error, CapturedError::Limit(_)), "{error}");
}

// AC-1078: the gap codes are the fixed ADR-0072 strings, in sorted order.
#[test]
fn the_gap_codes_are_fixed() {
    use ClosureGapReason::*;
    let codes = [
        ProducerNotParticipating,
        CrossFileInputUnbound,
        ExternalInputUnbound,
        RemoteReference,
        UpstreamClosureIncomplete,
        EnrichmentChangedFact,
        CacheReuseUnproven,
        LimitExceeded,
    ]
    .map(|code| serde_json::to_value(code).unwrap());
    assert_eq!(
        serde_json::Value::Array(codes.to_vec()),
        serde_json::json!([
            "producer_not_participating",
            "cross_file_input_unbound",
            "external_input_unbound",
            "remote_reference",
            "upstream_closure_incomplete",
            "enrichment_changed_fact",
            "cache_reuse_unproven",
            "limit_exceeded"
        ])
    );
}
