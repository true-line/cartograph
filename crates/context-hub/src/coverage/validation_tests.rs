use super::*;
use core_graph::{Node, coverage::CoverageRecord};
use serde_json::json;

fn fixture() -> (CoverageProof, StoredCoverage) {
    let record = CoverageRecord {
        repo_key: "acme/api".into(),
        registered_source_id: Some("src:acme/api".into()),
        ingest_id: "job:1".into(),
        config_hash: "config:one".into(),
        inventory_json: json!({"inventory":"coverage@1", "repo":"acme/api", "commit":"workdir",
            "layer_hints":["server"], "producers":[{"state":"not_applicable"}],
            "unsupported":[], "limited":false})
        .to_string(),
    };
    let stored = StoredCoverage {
        inventory_hash: record.inventory_hash(),
        record,
    };
    let qualification = CoverageQualification::new(
        &[ScopedRepo {
            repo_key: "acme/api".into(),
            registered_source_id: Some("src:acme/api".into()),
            config_hash: "config:one".into(),
            layer_hints: None,
        }],
        std::slice::from_ref(&stored),
    )
    .unwrap();
    let snapshot = ContextSnapshot::new(
        vec![Node {
            id: "repo:acme/api".into(),
            label: "Repo".into(),
            props: json!({}),
        }],
        vec![],
    )
    .unwrap();
    (
        snapshot
            .with_coverage(qualification)
            .unwrap()
            .durable_proof()
            .unwrap(),
        stored,
    )
}

fn reseal(proof: &mut CoverageProof) {
    proof.qualification.coverage_id = content_identity(
        "coverage-context-v1",
        &QualificationIdentity {
            contract: &proof.qualification.contract,
            exhaustive: proof.qualification.exhaustive,
            repos: &proof.qualification.repos,
            basis: &proof.basis,
        },
    )
    .unwrap();
    proof.context_id =
        context_identity(&proof.snapshot_id, &proof.qualification.coverage_id).unwrap();
}

// AC-0239: the proof retains identity inputs independently of the exact public
// wire and remains verifiable after all live inventory caches are lost.
#[test]
fn durable_proof_round_trips_without_changing_public_wire_or_identity() {
    let (proof, stored) = fixture();
    let public = serde_json::to_value(&proof.qualification).unwrap();
    assert_eq!(public.as_object().unwrap().len(), 4);
    assert!(public.get("basis").is_none());
    assert!(public.get("inventories").is_none());
    // This is the pre-proof identity shape, including its original field names.
    let original_identity = json!({"contract":COVERAGE_CONTRACT,"exhaustive":false,
        "repos":proof.qualification.repos,"basis":[{"repo":"acme/api",
        "registered_source_id":"src:acme/api","ingest_id":"job:1",
        "config_hash":"config:one","inventory_hash":stored.inventory_hash}]});
    assert_eq!(
        proof.qualification.coverage_id,
        content_identity("coverage-context-v1", &original_identity).unwrap()
    );
    let raw = serde_json::to_string(&proof).unwrap();
    let restored: CoverageProof = serde_json::from_str(&raw).unwrap();
    assert_eq!(restored, proof);
    restored.validate().unwrap();
    restored.validate_inventory(&stored).unwrap();
    assert!(
        restored
            .qualification
            .inventory("acme/api", &stored.inventory_hash)
            .is_err()
    );
    assert!(
        restored
            .qualification
            .durable_proof(&restored.snapshot_id)
            .is_err()
    );
}

// AC-0239: every recorded basis component, public block and scoped context ID
// participates in historical consistency; a same-graph replacement cannot pass.
#[test]
fn durable_proof_rejects_independent_identity_tampering() {
    let (proof, _) = fixture();
    let mut changes = Vec::new();
    let mut changed = proof.clone();
    changed.schema_version = 2;
    changes.push(changed);
    let mut changed = proof.clone();
    changed.qualification.coverage_id.push('0');
    changes.push(changed);
    let mut changed = proof.clone();
    changed.context_id.push('0');
    changes.push(changed);
    let mut changed = proof.clone();
    changed.snapshot_id = format!("context-v1:{}", "0".repeat(64));
    changes.push(changed);
    let mut changed = proof.clone();
    changed.basis[0].ingest_id = "job:2".into();
    changes.push(changed);
    let mut changed = proof.clone();
    changed.basis[0].config_hash = "config:two".into();
    changes.push(changed);
    let mut changed = proof.clone();
    changed.basis[0].registered_source_id = Some("src:other".into());
    changes.push(changed);
    let mut changed = proof.clone();
    changed.basis[0].inventory_hash = "0".repeat(64);
    changes.push(changed);
    let mut changed = proof.clone();
    changed.qualification.repos[0].commit = Some("other".into());
    changes.push(changed);
    for changed in changes {
        assert!(changed.validate().is_err());
    }
}

// AC-0239: recomputing attacker-controlled hashes cannot make an impossible
// status shape, duplicate repo, noncanonical ordering or unbounded identity valid.
#[test]
fn durable_proof_validates_shapes_even_when_hashes_are_recomputed() {
    let (proof, _) = fixture();
    let mut changes = Vec::new();
    let mut changed = proof.clone();
    changed.qualification.exhaustive = true;
    changes.push(changed);
    let mut changed = proof.clone();
    changed.qualification.contract = "coverage-qualification@2".into();
    changes.push(changed);
    let mut changed = proof.clone();
    changed.qualification.repos[0].status = CoverageStatus::Stale;
    changes.push(changed);
    let mut changed = proof.clone();
    changed.qualification.repos[0].status = CoverageStatus::Unrecorded;
    changes.push(changed);
    let mut changed = proof.clone();
    changed.qualification.repos[0].counts = None;
    changes.push(changed);
    let mut changed = proof.clone();
    changed.qualification.repos[0].limited = Some(true);
    changes.push(changed);
    let mut changed = proof.clone();
    changed.qualification.repos[0].compared.reverse();
    changes.push(changed);
    let mut changed = proof.clone();
    changed.qualification.repos[0]
        .compared
        .push("inputs".into());
    changes.push(changed);
    let mut changed = proof.clone();
    changed.qualification.repos[0].layer_hints = Some(vec!["server".into(), "infra".into()]);
    changes.push(changed);
    let mut changed = proof.clone();
    changed.qualification.repos[0].layer_hints = Some(vec!["server".into(), "server".into()]);
    changes.push(changed);
    let mut changed = proof.clone();
    changed
        .qualification
        .repos
        .push(changed.qualification.repos[0].clone());
    changes.push(changed);
    let mut changed = proof.clone();
    changed.basis.push(changed.basis[0].clone());
    changes.push(changed);
    let mut changed = proof.clone();
    changed.basis[0].registered_source_id = None;
    changes.push(changed);
    let mut changed = proof.clone();
    changed.basis[0].ingest_id = "a".repeat(MAX_COVERAGE_IDENTITY_BYTES + 1);
    changes.push(changed);
    let mut changed = proof.clone();
    changed.basis[0].config_hash = "config\0hidden".into();
    changes.push(changed);
    let mut changed = proof.clone();
    changed.qualification.repos[0]
        .counts
        .as_mut()
        .unwrap()
        .missing_input = usize::MAX;
    changed.qualification.repos[0].limited = Some(true);
    changes.push(changed);
    for mut changed in changes {
        reseal(&mut changed);
        assert!(changed.validate().is_err());
    }
}

// AC-0239: stale/missing coverage remains historically meaningful but never
// grants detail access or permits inventing inventory-derived fields.
#[test]
fn durable_proof_preserves_stale_and_unrecorded_without_granting_details() {
    let (original, stored) = fixture();
    let qualification = CoverageQualification::new(
        &[ScopedRepo {
            repo_key: "acme/new".into(),
            registered_source_id: Some("src:new".into()),
            config_hash: "config:one".into(),
            layer_hints: None,
        }],
        std::slice::from_ref(&stored),
    )
    .unwrap();
    let proof = qualification.durable_proof(&original.snapshot_id).unwrap();
    assert_eq!(proof.qualification.repos[0].status, CoverageStatus::Stale);
    assert_eq!(
        proof.qualification.repos[1].status,
        CoverageStatus::Unrecorded
    );
    proof.validate().unwrap();
    assert!(matches!(
        proof.validate_inventory(&stored),
        Err(ContextError::InventoryUnavailable(_))
    ));
    let mut changed = proof.clone();
    changed.qualification.repos[1].counts = Some(CoverageCounts::default());
    reseal(&mut changed);
    assert!(changed.validate().is_err());
    let mut changed = proof;
    changed.qualification.repos.swap(0, 1);
    reseal(&mut changed);
    assert!(changed.validate().is_err());
}

// AC-0239: archived originals bind body bytes and every ingest identity. A body
// with a valid hash must still agree with the qualification's derived summary.
#[test]
fn retained_inventory_validates_original_basis_hash_and_derived_fields() {
    let (proof, stored) = fixture();
    let mut changed = stored.clone();
    changed.record.ingest_id = "job:2".into();
    assert!(proof.validate_inventory(&changed).is_err());
    let mut changed = stored.clone();
    changed.record.registered_source_id = Some("src:other".into());
    assert!(proof.validate_inventory(&changed).is_err());
    let mut changed = stored.clone();
    changed.record.config_hash = "other".into();
    assert!(proof.validate_inventory(&changed).is_err());
    let mut changed = stored.clone();
    changed.record.inventory_json.push(' ');
    assert!(proof.validate_inventory(&changed).is_err());
    let mut changed = stored;
    let mut body: Value = serde_json::from_str(&changed.record.inventory_json).unwrap();
    body["producers"][0]["state"] = json!("missing_input");
    body["limited"] = json!(true);
    changed.record.inventory_json = body.to_string();
    changed.inventory_hash = changed.record.inventory_hash();
    let mut rebound = proof;
    rebound.basis[0].inventory_hash = changed.inventory_hash.clone();
    rebound.qualification.repos[0].inventory_hash = Some(changed.inventory_hash.clone());
    reseal(&mut rebound);
    rebound.validate().unwrap();
    assert!(rebound.validate_inventory(&changed).is_err());
}

// AC-0239: unknown fields at every persisted proof layer are rejected rather
// than disappearing before hash validation.
#[test]
fn durable_proof_decoding_rejects_unknown_fields() {
    let (proof, _) = fixture();
    for path in [
        vec![],
        vec!["qualification"],
        vec!["basis", "0"],
        vec!["qualification", "repos", "0"],
        vec!["qualification", "repos", "0", "counts"],
    ] {
        let mut value = serde_json::to_value(&proof).unwrap();
        let mut target = &mut value;
        for key in path {
            target = if key == "0" {
                &mut target[0]
            } else {
                &mut target[key]
            };
        }
        target
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), json!(true));
        assert!(serde_json::from_value::<CoverageProof>(value).is_err());
    }
}

// AC-0239: archive admission checks one proof and requires all and only its
// Recorded originals, independent of archive order, without duplicate substitutes.
#[test]
fn retained_inventory_batch_requires_exact_recorded_set() {
    let (first, stored) = fixture();
    let mut other = stored.clone();
    other.record.repo_key = "acme/web".into();
    other.record.registered_source_id = Some("src:acme/web".into());
    let mut body: Value = serde_json::from_str(&other.record.inventory_json).unwrap();
    body["repo"] = json!("acme/web");
    other.record.inventory_json = body.to_string();
    other.inventory_hash = other.record.inventory_hash();
    let scope = [&stored, &other].map(|stored| ScopedRepo {
        repo_key: stored.record.repo_key.clone(),
        registered_source_id: stored.record.registered_source_id.clone(),
        config_hash: stored.record.config_hash.clone(),
        layer_hints: None,
    });
    let proof = CoverageQualification::new(&scope, &[stored.clone(), other.clone()])
        .unwrap()
        .durable_proof(&first.snapshot_id)
        .unwrap();
    proof
        .validate_inventories(&[other.clone(), stored.clone()])
        .unwrap();
    assert!(proof.validate_inventories(&[]).is_err());
    assert!(
        proof
            .validate_inventories(std::slice::from_ref(&stored))
            .is_err()
    );
    assert!(
        proof
            .validate_inventories(&[stored.clone(), stored.clone()])
            .is_err()
    );
    other.record.ingest_id = "job:other".into();
    assert!(proof.validate_inventories(&[stored, other]).is_err());
    let empty = CoverageQualification::new(&[], &[])
        .unwrap()
        .durable_proof(&first.snapshot_id)
        .unwrap();
    empty.validate_inventories(&[]).unwrap();
}
