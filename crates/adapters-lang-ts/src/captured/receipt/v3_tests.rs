use super::*;

const CODE: &[u8] = b"function run(input:any){ const base = input.enabled; const ready = base === false; if (ready) return false; }";

pub(super) fn captured_rule_receipt() -> (Node, Receipt) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("source.ts"), CODE).unwrap();
    let source = source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap();
    let capture = source_capture::capture_working_tree(
        dir.path(),
        &source,
        &["source.ts".into()],
        source_capture::CaptureLimits::default(),
    )
    .unwrap();
    let (facts, receipts) = super::super::extract_file(
        capture.file("source.ts").unwrap(),
        &SourceId {
            repo: "acme/project",
            commit: "workdir",
        },
    )
    .unwrap();
    let node = facts
        .nodes
        .into_iter()
        .find(|node| node.label == "BusinessRule")
        .unwrap();
    let receipt = receipts
        .into_iter()
        .find(|receipt| receipt.fact_key() == &FactKey::from_node(&node))
        .unwrap();
    (node, receipt)
}

fn as_v2(v3: &Receipt) -> Receipt {
    let mut content = v3.content.clone();
    content.schema_version = 2;
    content.producer_contract = "t0.adapter-ts/direct-lexical-v2".into();
    content.commit_sha = None;
    Receipt {
        receipt_id: content_id(&content).unwrap(),
        content,
    }
}

pub(super) fn sign_wire(mut wire: serde_json::Value, version: u32) -> String {
    let contract = contract(version, Grammar::Tsx).unwrap();
    let mut input = contract.domain.to_vec();
    input.extend(serde_json::to_vec(&canonical(wire["content"].clone())).unwrap());
    wire["receipt_id"] = serde_json::Value::String(format!(
        "{}{}",
        contract.prefix,
        core_prov::content_hash(&input)
    ));
    serde_json::to_string(&wire).unwrap()
}

#[test]
fn receipt_v2_wire_identity_and_order_remain_unchanged() {
    let file = format!(
        "{{\"source_id\":\"src_{}\",\"capture_id\":\"capture-v1:{}\",\"path\":\"source.ts\",\"digest\":\"{}\",\"byte_len\":5}}",
        "1".repeat(32),
        "c".repeat(64),
        "a".repeat(64)
    );
    let content_json = format!(
        concat!(
            "{{\"schema_version\":2,\"source_id\":\"src_{}\",\"repo_key\":\"acme/project\",\"file\":{},",
            "\"producer_contract\":\"t0.adapter-ts/direct-lexical-v2\",\"grammar\":\"typescript\",",
            "\"grammar_package\":\"tree-sitter-typescript@0.23.2\",\"parser_package\":\"tree-sitter@0.27.0\",",
            "\"fact_key\":{{\"kind\":\"node\",\"id\":\"symbol:historical\"}},\"fact_digest\":\"node-v1:{}\",",
            "\"ranges\":[{{\"role\":\"provenance\",\"index\":0,\"evidence\":{{\"repo\":\"acme/project\",",
            "\"path\":\"source.ts\",\"byte_start\":0,\"byte_end\":5,\"commit_sha\":\"historical\"}},",
            "\"captured\":{{\"file\":{},\"byte_start\":0,\"byte_end\":5}}}}],",
            "\"source_scope\":\"primary_source_only\",\"input_closure\":\"input_closure_not_established\"}}"
        ),
        "1".repeat(32),
        file,
        "b".repeat(64),
        file
    );
    let content: serde_json::Value = serde_json::from_str(&content_json).unwrap();
    let mut bytes = b"cartograph:ts-primary-receipt:v2\0".to_vec();
    bytes.extend(serde_json::to_vec(&content).unwrap());
    let expected_id = format!("ts-primary-v2:{}", core_prov::content_hash(&bytes));
    assert_eq!(
        expected_id,
        "ts-primary-v2:b9e95053d1c7a350b414225424f69736a283820028d18066f9f4a78ae5f5d1ba"
    );
    let original = format!("{{\"receipt_id\":\"{expected_id}\",\"content\":{content_json}}}");
    let decoded = Receipt::from_json(&original).unwrap();
    assert_eq!(decoded.id(), expected_id);
    assert_eq!(decoded.to_json().unwrap(), original);
}

#[test]
fn receipt_v3_parity_round_trip_and_size_reduction() {
    let (node, v3) = captured_rule_receipt();
    let v2 = as_v2(&v3);

    v2.validate().unwrap();
    v3.validate().unwrap();
    assert_eq!(v2.ranges(), v3.ranges());
    assert!(v2.matches_node(&node));
    assert!(v3.matches_node(&node));
    assert_ne!(v2.id(), v3.id());
    assert!(v2.id().starts_with("ts-primary-v2:"));
    assert!(v3.id().starts_with("ts-primary-v3:"));

    let v2_json = v2.to_json().unwrap();
    let v3_json = v3.to_json().unwrap();
    assert!(
        v3_json.len() * 3 <= v2_json.len(),
        "expected v3 ({} bytes) to be at least 3x smaller than v2 ({} bytes)",
        v3_json.len(),
        v2_json.len()
    );

    let decoded_v3 = Receipt::from_json(&v3_json).unwrap();
    assert_eq!(decoded_v3, v3);
    assert_eq!(decoded_v3.to_json().unwrap(), v3_json);

    let decoded_v2 = Receipt::from_json(&v2_json).unwrap();
    assert_eq!(decoded_v2, v2);
    assert_eq!(decoded_v2.to_json().unwrap(), v2_json);
}

#[test]
fn receipt_v3_rejects_malformed_wire_and_oversized_expansions() {
    let (_, v3) = captured_rule_receipt();
    let v2 = as_v2(&v3);
    let v3_wire = serde_json::to_value(&v3).unwrap();
    let v2_wire = serde_json::to_value(&v2).unwrap();

    // 1. A v3 range with an extra field.
    let mut extra_range_field = v3_wire.clone();
    extra_range_field["content"]["ranges"][0]["extra"] = serde_json::json!(true);
    assert!(Receipt::from_json(&sign_wire(extra_range_field, 3)).is_err());

    // 2. A v3 range in v2 full form.
    let mut full_range_in_v3 = v3_wire.clone();
    full_range_in_v3["content"]["ranges"][0] = serde_json::to_value(&v3.ranges()[0]).unwrap();
    assert!(Receipt::from_json(&sign_wire(full_range_in_v3, 3)).is_err());

    // 3. A v2 payload with a compact range.
    let mut compact_range_in_v2 = v2_wire.clone();
    compact_range_in_v2["content"]["ranges"][0] = v3_wire["content"]["ranges"][0].clone();
    assert!(Receipt::from_json(&sign_wire(compact_range_in_v2, 2)).is_err());

    // 4. A v2 payload with commit_sha.
    let mut v2_with_commit = v2.clone();
    v2_with_commit.content.commit_sha = Some("workdir".into());
    v2_with_commit.receipt_id = content_id(&v2_with_commit.content).unwrap();
    assert!(v2_with_commit.validate().is_err());
    assert!(Receipt::from_json(&serde_json::to_string(&v2_with_commit).unwrap()).is_err());

    // 5. A v3 payload missing commit_sha.
    let mut v3_missing_commit = v3.clone();
    v3_missing_commit.content.commit_sha = None;
    v3_missing_commit.receipt_id = content_id(&v3_missing_commit.content).unwrap();
    assert!(v3_missing_commit.validate().is_err());
    assert!(Receipt::from_json(&serde_json::to_string(&v3_missing_commit).unwrap()).is_err());

    // 6. A v3 commit_sha that is empty, oversized, or has control characters.
    let oversized_sha = "a".repeat(MAX_ID_BYTES + 1);
    for bad_sha in ["", "bad\nsha", "bad\0sha", oversized_sha.as_str()] {
        let mut bad_commit = v3.clone();
        bad_commit.content.commit_sha = Some(bad_sha.into());
        for range in &mut bad_commit.content.ranges {
            range.evidence.commit_sha = bad_sha.into();
        }
        bad_commit.receipt_id = content_id(&bad_commit.content).unwrap();
        assert!(bad_commit.validate().is_err());
        assert!(Receipt::from_json(&serde_json::to_string(&bad_commit).unwrap()).is_err());
    }

    // 7. Duplicate (role, index).
    let mut duplicate = v3.clone();
    let first = duplicate.ranges()[0].clone();
    duplicate.content.ranges = vec![first.clone(), first];
    duplicate.receipt_id = content_id(&duplicate.content).unwrap();
    assert!(duplicate.validate().is_err());
    assert!(Receipt::from_json(&serde_json::to_string(&duplicate).unwrap()).is_err());

    // 8. More than MAX_RECEIPT_RANGES compact ranges.
    let mut too_many_ranges = v3.clone();
    too_many_ranges.content.ranges = (0..=MAX_RECEIPT_RANGES)
        .map(|index| {
            let mut range = v3.ranges()[0].clone();
            range.role = RangeRole::Provenance;
            range.index = index as u32;
            range
        })
        .collect();
    too_many_ranges.receipt_id = content_id(&too_many_ranges.content).unwrap();
    let too_many_json = serde_json::to_string(&too_many_ranges).unwrap();
    assert!(too_many_json.len() <= MAX_RECEIPT_BYTES);
    assert!(matches!(
        too_many_ranges.validate(),
        Err(CapturedError::Limit("receipt ranges"))
    ));
    assert!(Receipt::from_json(&too_many_json).is_err());

    // 9. A v3 receipt whose compact form fits within MAX_RECEIPT_BYTES but
    // whose expanded (v2-shaped) form exceeds MAX_RECEIPT_BYTES.
    let mut oversized_expansion = too_many_ranges;
    oversized_expansion
        .content
        .ranges
        .truncate(MAX_RECEIPT_RANGES);
    oversized_expansion.receipt_id = content_id(&oversized_expansion.content).unwrap();
    let compact_json = serde_json::to_string(&oversized_expansion).unwrap();
    let expanded_bytes = serde_json::to_vec(&ExpandedReceipt {
        receipt_id: &oversized_expansion.receipt_id,
        content: oversized_expansion.content.as_full_wire(None),
    })
    .unwrap()
    .len();
    assert!(compact_json.len() <= MAX_RECEIPT_BYTES);
    assert!(expanded_bytes > MAX_RECEIPT_BYTES);
    assert!(matches!(
        oversized_expansion.validate(),
        Err(CapturedError::Limit("receipt bytes"))
    ));
    assert!(oversized_expansion.to_json().is_err());
    assert!(Receipt::from_json(&compact_json).is_err());
}

#[test]
fn receipt_wire_rejects_duplicate_keys_and_explicit_null_commit() {
    // A duplicated key or an explicit `null` must not become a second wire
    // encoding of the same receipt id, in any contract version.
    let (_, v3) = captured_rule_receipt();
    let v2 = as_v2(&v3);
    for receipt in [&v2, &v3] {
        let json = receipt.to_json().unwrap();
        assert!(Receipt::from_json(&json).is_ok());
        let repo = format!(
            "\"repo_key\":{}",
            serde_json::to_string(receipt.repo_key()).unwrap()
        );
        let duplicated = json.replacen(&repo, &format!("{repo},{repo}"), 1);
        assert_ne!(duplicated, json);
        assert!(Receipt::from_json(&duplicated).is_err());
        let range_role = "\"role\":\"provenance\"";
        let duplicated_in_range =
            json.replacen(range_role, &format!("{range_role},{range_role}"), 1);
        assert_ne!(duplicated_in_range, json);
        assert!(Receipt::from_json(&duplicated_in_range).is_err());
    }
    let mut null_commit = serde_json::to_value(&v2).unwrap();
    null_commit["content"]["commit_sha"] = serde_json::Value::Null;
    assert!(Receipt::from_json(&serde_json::to_string(&null_commit).unwrap()).is_err());
}

#[test]
fn expansion_preflight_is_exact_for_the_ranges_validate_measures() {
    // #745 review: the decoder sizes the v2-shaped expansion before cloning
    // shared identity into each range. It must equal the real serialization,
    // so it neither admits an oversized expansion nor rejects a valid receipt.
    let (_, v3) = captured_rule_receipt();
    let wire: Vec<WireRange> = v3
        .ranges()
        .iter()
        .map(|range| {
            WireRange::Compact(CompactRange {
                role: range.role,
                index: range.index,
                byte_start: range.evidence.byte_start,
                byte_end: range.evidence.byte_end,
                file: None,
            })
        })
        .collect();
    let commit = v3.content.commit_sha.as_deref().unwrap();
    assert_eq!(
        expanded_range_bytes(&wire, v3.repo_key(), v3.file(), &[], commit),
        Some(serde_json::to_vec(v3.ranges()).unwrap().len())
    );
    assert_eq!(
        expanded_range_bytes(&[], v3.repo_key(), v3.file(), &[], commit),
        Some(2)
    );
    let mixed = vec![WireRange::Full(v3.ranges()[0].clone())];
    assert_eq!(
        expanded_range_bytes(&mixed, v3.repo_key(), v3.file(), &[], commit),
        None
    );
}

fn string_bytes(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::String(text) => text.len(),
        serde_json::Value::Array(values) => values.iter().map(string_bytes).sum(),
        serde_json::Value::Object(values) => values
            .iter()
            .map(|(key, value)| key.len() + string_bytes(value))
            .sum(),
        _ => 0,
    }
}

#[test]
fn fact_match_workspace_covers_real_match_and_rejected_inventories() {
    // AC-0327: one model covers a real rule fact's key, digest and inventory
    // owners, including a corrupt rule and provenance past the range cap.
    let (node, receipt) = captured_rule_receipt();
    assert!(receipt.matches_node(&node));
    let wire = serde_json::to_vec(&node).unwrap().len();
    // The digest re-encodes the same values, so its body is the fact wire.
    let encoded = serde_json::to_vec(&canonical(serde_json::to_value(&node).unwrap()));
    assert_eq!(encoded.unwrap().len(), wire);
    let provenance: Provenance = serde_json::from_value(node.props["prov"].clone()).unwrap();
    let rule = GuardedExitEvidence::from_value(node.props["rule"].clone()).unwrap();
    let owners = std::mem::size_of::<FactKey>()
        + node.id.len()
        + std::mem::size_of::<Provenance>()
        + std::mem::size_of_val(provenance.evidence.as_slice())
        + string_bytes(&node.props["prov"])
        + std::mem::size_of_val(receipt.ranges())
        + std::mem::size_of_val(&rule)
        + std::mem::size_of_val(rule.conditions.as_slice())
        + string_bytes(&node.props["rule"]);
    let model = fact_match_workspace_bytes(wire).unwrap();
    assert!(model >= owners + wire);
    assert!(model >= core_graph::rules::validation_workspace_bytes(wire).unwrap());

    let mut corrupt = node.clone();
    corrupt.props["rule"]["owner_id"] = "".into();
    assert!(!receipt.matches_node(&corrupt));
    assert!(GuardedExitEvidence::from_value(corrupt.props["rule"].clone()).is_err());
    assert!(matches!(
        inventory(
            &corrupt.props,
            receipt.content.schema_version,
            receipt.content.grammar
        ),
        Err(CapturedError::Invalid(_))
    ));
    assert!(fact_match_workspace_bytes(serde_json::to_vec(&corrupt).unwrap().len()).is_some());

    // Provenance past the range cap decodes in full before the refusal.
    let mut large = node.clone();
    let evidence = large.props["prov"]["evidence"][0].clone();
    large.props["prov"]["evidence"] = vec![evidence; MAX_RECEIPT_RANGES + 1].into();
    assert!(matches!(
        inventory(
            &large.props,
            receipt.content.schema_version,
            receipt.content.grammar
        ),
        Err(CapturedError::Limit(_))
    ));
    assert!(!receipt.matches_node(&large));
    let wire = serde_json::to_vec(&large).unwrap().len();
    let decoded = (MAX_RECEIPT_RANGES + 1) * std::mem::size_of::<EvidenceRef>()
        + string_bytes(&large.props["prov"])
        + MAX_RECEIPT_RANGES * std::mem::size_of::<(RangeRole, u32, EvidenceRef)>();
    assert!(fact_match_workspace_bytes(wire).unwrap() >= decoded + wire);

    assert!(
        fact_match_workspace_bytes(wire + 1).unwrap() > fact_match_workspace_bytes(wire).unwrap()
    );
    assert!(fact_match_workspace_bytes(usize::MAX).is_none());
    assert!(fact_match_workspace_bytes(usize::MAX / 4).is_none());
}

/// Values counted like `SnapshotReadLimits::max_values`: keys are not values.
fn value_count(value: &serde_json::Value) -> usize {
    1 + match value {
        serde_json::Value::Array(values) => values.iter().map(value_count).sum(),
        serde_json::Value::Object(values) => values.values().map(value_count).sum(),
        _ => 0,
    }
}

#[test]
fn fact_match_value_bound_tightens_the_wire_model_on_real_inputs() {
    // AC-0327: an admitted value count only replaces the one-value-per-byte
    // assumption; it still covers real valid and malformed matches.
    let (node, receipt) = captured_rule_receipt();
    let fact = serde_json::to_value(&node).unwrap();
    let wire = serde_json::to_vec(&fact).unwrap().len();
    let values = value_count(&fact);
    let wire_only = fact_match_workspace_bytes(wire).unwrap();
    assert_eq!(fact_match_workspace(wire, wire + 1), Some(wire_only));
    assert_eq!(fact_match_workspace(wire, usize::MAX), Some(wire_only));
    let model = fact_match_workspace(wire, values).unwrap();
    assert!(model < wire_only);
    let provenance: Provenance = serde_json::from_value(node.props["prov"].clone()).unwrap();
    let rule = GuardedExitEvidence::from_value(node.props["rule"].clone()).unwrap();
    assert!(
        model
            >= std::mem::size_of::<FactKey>()
                + node.id.len()
                + std::mem::size_of_val(provenance.evidence.as_slice())
                + std::mem::size_of_val(receipt.ranges())
                + std::mem::size_of_val(&rule)
                + values * std::mem::size_of::<serde_json::Value>()
                + 2 * wire
    );
    assert!(model >= core_graph::rules::validation_workspace(wire, values).unwrap());

    // A malformed rule member is counted and still refused.
    let mut malformed = node.clone();
    let row: serde_json::Value = vec![serde_json::Value::Null; 20].into();
    malformed.props["rule"]["untrusted"] = vec![row; 1_000].into();
    assert!(!receipt.matches_node(&malformed));
    assert!(matches!(
        inventory(
            &malformed.props,
            receipt.content.schema_version,
            receipt.content.grammar
        ),
        Err(CapturedError::Invalid(_))
    ));
    let fact = serde_json::to_value(&malformed).unwrap();
    let wire = serde_json::to_vec(&fact).unwrap().len();
    let values = value_count(&fact);
    let model = fact_match_workspace(wire, values).unwrap();
    assert!(model < fact_match_workspace_bytes(wire).unwrap());
    assert!(model >= values * std::mem::size_of::<(&serde_json::Value, usize)>() + wire);

    assert!(fact_match_workspace(usize::MAX, 1).is_none());
    assert!(fact_match_workspace(usize::MAX / 4, usize::MAX / 4).is_none());
}

// AC-1024, AC-1026, AC-1029, AC-1031, AC-1033, AC-1035: each lane's
// receipts name the exact grammar and parser pins its adapter manifest
// declares.
#[test]
fn lane_pins_match_the_adapter_manifests() {
    let python = include_str!("../../../../adapters-lang-python/Cargo.toml");
    let go = include_str!("../../../../adapters-lang-go/Cargo.toml");
    let java = include_str!("../../../../adapters-lang-java/Cargo.toml");
    let rust = include_str!("../../../../adapters-lang-rust/Cargo.toml");
    let kotlin = include_str!("../../../../adapters-lang-kotlin/Cargo.toml");
    let terraform = include_str!("../../../../iac/Cargo.toml");
    let ts = include_str!("../../../Cargo.toml");
    for (manifest, grammar, package) in [
        (ts, Grammar::TypeScript, "tree-sitter-typescript"),
        (python, Grammar::Python, "tree-sitter-python"),
        (go, Grammar::Go, "tree-sitter-go"),
        (java, Grammar::Java, "tree-sitter-java"),
        (rust, Grammar::Rust, "tree-sitter-rust"),
        (kotlin, Grammar::Kotlin, "tree-sitter-kotlin-ng"),
        (terraform, Grammar::Terraform, "hcl-edit"),
    ] {
        let contract = contract(3, grammar).unwrap();
        let (name, version) = contract.grammar_package.split_once('@').unwrap();
        assert_eq!(name, package);
        assert!(manifest.contains(&format!("{name} = \"={version}\"")));
        let (name, version) = contract.parser_package.split_once('@').unwrap();
        assert!(manifest.contains(&format!("{name} = \"={version}\"")));
    }
}
