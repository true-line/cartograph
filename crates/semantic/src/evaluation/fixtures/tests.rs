use super::*;
use serde_json::json;
// AC-0272: retain the independently authored fixture files exactly as published
// by trueline-claude-agent[bot] in 24037d061be6278e9dd283b620a20ad85677d85e.
#[test]
fn fixture_import_matches_independent_author_frozen_file_listing() {
    let listing = include_str!("file-hashes.txt");
    assert_eq!(
        sha256(listing.as_bytes()),
        "5faae80347605270b7c35d328b6007cc6657353b320a8e6267cc513123e7a6dd"
    );
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/t2-visible-mechanism-v1");
    let expected: std::collections::BTreeMap<_, _> = listing
        .lines()
        .map(|line| {
            let (hash, path) = line.split_once("  ").unwrap();
            (path.strip_prefix("./").unwrap().to_owned(), hash)
        })
        .collect();
    fn files(
        root: &std::path::Path,
        dir: &std::path::Path,
        out: &mut std::collections::BTreeSet<String>,
    ) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            assert!(!kind.is_symlink());
            if kind.is_dir() {
                files(root, &entry.path(), out);
            } else {
                assert!(kind.is_file());
                out.insert(
                    entry
                        .path()
                        .strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut actual = std::collections::BTreeSet::new();
    files(&root, &root, &mut actual);
    assert_eq!(actual, expected.keys().cloned().collect());
    for (path, hash) in expected {
        assert_eq!(
            sha256(&std::fs::read(root.join(&path)).unwrap()),
            hash,
            "{path}"
        );
    }
}
// AC-0271: raw admission rejects duplicate and escaped duplicate keys at any
// nesting level before serde can discard them. No fixture truth is invented.
#[test]
fn fixture_raw_parser_rejects_duplicate_keys_and_noninteger_numbers() {
    for raw in [
        r#"{"kind":"Call","kind":"Channel","subtype":null}"#,
        r#"{"kind":"Call","\u006bind":"Channel","subtype":null}"#,
        r#"{"nested":[{"x":1,"x":2}]}"#,
        r#"{"kind":"Call","subtype":1.0}"#,
        r#"{"kind":"Call","subtype":1e0}"#,
        r#"{"kind":"Call","subtype":"\ud800"}"#,
    ] {
        assert!(canonical::decode::<FamilyKey>(raw.as_bytes(), "FamilyKey").is_err());
    }
}
// AC-0271: nullable subtype is required and closed, not absent-as-null.
#[test]
fn fixture_raw_parser_requires_nullable_keys_and_closed_shapes() {
    for raw in [
        r#"{"kind":"Call"}"#,
        r#"{"kind":"Call","subtype":null,"passed":true}"#,
        r#"{"kind":"Other","subtype":null}"#,
    ] {
        assert!(canonical::decode::<FamilyKey>(raw.as_bytes(), "FamilyKey").is_err());
    }
    let f: FamilyKey =
        canonical::decode(br#"{"kind":"Channel","subtype":null}"#, "FamilyKey").unwrap();
    assert_eq!(f, FamilyKey::new(crate::HopKind::Channel, None));
    let oversized = vec![b' '; MAX_MANIFEST_BYTES + 1];
    assert!(matches!(
        canonical::decode::<InputsManifest>(&oversized, "InputsManifest"),
        Err(FixtureError::Capacity)
    ));
}
// AC-0271: integer-only JCS preserves Unicode values/control escaping while
// sorting ASCII schema keys. This is an algorithm vector, not a labeled case.
#[test]
fn fixture_jcs_integer_strings_and_ascii_keys_have_literal_bytes() {
    let value = json!({"z":[0,16777216,true,null],"a":"/\u{2028}\né😀"});
    assert_eq!(
        canonical_json(&value).unwrap(),
        "{\"a\":\"/\u{2028}\\né😀\",\"z\":[0,16777216,true,null]}".as_bytes()
    );
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(canonical_json(&json!({"x":1.5})).is_err());
    assert!(canonical_json(&json!({"x":9_007_199_254_740_992_u64})).is_err());
    assert!(canonical_json(&json!({"é":1})).is_err());
}
// AC-0271: no-follow descendant paths reject traversal and absolute paths.
#[test]
fn fixture_paths_and_missing_or_unbounded_manifests_fail_closed() {
    for path in [
        "/root", "../x", "a/../b", "a/./b", "a//b", "a\\b", "a:b", "a\0b", "a\nb",
    ] {
        assert!(!validation::path(path));
    }
    let dir = tempfile::tempdir().unwrap();
    assert!(load_fixture_bundle(dir.path()).is_err());
    for name in ["inputs.json", "leakage.json", "labels.json"] {
        std::fs::write(dir.path().join(name), b"{}").unwrap();
    }
    std::fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join("inputs.json"))
        .unwrap()
        .set_len(MAX_MANIFEST_BYTES as u64 + 1)
        .unwrap();
    assert!(matches!(
        load_fixture_bundle(dir.path()),
        Err(FixtureError::Capacity)
    ));
}
#[cfg(unix)]
// AC-0271: selected symlink is never read, even when target is a regular file.
#[test]
fn fixture_manifest_symlink_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("real.json"), b"{}").unwrap();
    std::os::unix::fs::symlink("real.json", dir.path().join("inputs.json")).unwrap();
    assert!(load_fixture_bundle(dir.path()).is_err());
}

fn authored_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/t2-visible-mechanism-v1")
}
fn copy_authored() -> tempfile::TempDir {
    fn copy(src: &std::path::Path, dst: &std::path::Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let dest = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &dest);
            } else {
                std::fs::copy(entry.path(), dest).unwrap();
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    copy(&authored_root(), dir.path());
    dir
}
fn documents(root: &std::path::Path) -> (serde_json::Value, serde_json::Value, serde_json::Value) {
    let read =
        |name: &str| serde_json::from_slice(&std::fs::read(root.join(name)).unwrap()).unwrap();
    (
        read("inputs.json"),
        read("leakage.json"),
        read("labels.json"),
    )
}
fn write_documents(
    root: &std::path::Path,
    i: &serde_json::Value,
    l: &mut serde_json::Value,
    b: &mut serde_json::Value,
) {
    let input = canonical_json(i).unwrap();
    let ih = sha256(&input);
    l["inputs_hash"] = json!(ih);
    b["inputs_hash"] = json!(ih);
    let leakage = canonical_json(l).unwrap();
    b["leakage_hash"] = json!(sha256(&leakage));
    for (name, bytes) in [
        ("inputs.json", input),
        ("leakage.json", leakage),
        ("labels.json", canonical_json(b).unwrap()),
    ] {
        std::fs::write(root.join(name), bytes).unwrap();
    }
}
// AC-0271: independently authored Claude fixtures establish interoperability,
// not real-model quality. Literal hashes were frozen before loader validation.
#[test]
fn fixture_independent_visible_bundle_matches_frozen_hashes_and_families() {
    assert_eq!(
        sha256(include_bytes!("schema.json")),
        "6d0477903fabc2f08ece74e1a79049374fa0262570e24b4a40aee4143c087428"
    );
    let b = load_fixture_bundle(&authored_root()).unwrap();
    assert_eq!(
        b.digests().inputs,
        "d0ab80d3370c4d1efe837f7477ba6e50c4c9608ecb513edf5268656f80104b2a"
    );
    assert_eq!(
        b.digests().leakage,
        "a4e98dba73e6e54741d512f7fc0c712dc7afb1b0195e86fb061a581eedc61c22"
    );
    assert_eq!(
        b.digests().labels,
        "ed712abd7ee632c2ac3e482edf93e2ab6c701fa117b1a67b66db1b3f61fd1ae7"
    );
    assert_eq!(b.inputs().cases.len(), 10);
    assert_eq!(b.inputs().sources.len(), 29);
    assert_eq!(b.unverified_corpus_references().len(), 2);
    let families: std::collections::BTreeSet<_> =
        b.inputs().cases.iter().map(|c| &c.family).collect();
    assert_eq!(families.len(), 4);
    assert!(
        b.labels()
            .labels
            .iter()
            .any(|l| matches!(l.truth, Truth::Unknown { .. }))
    );
    assert!(
        b.leakage()
            .groups
            .iter()
            .any(|g| g.split == Split::Calibration)
    );
    assert!(
        b.leakage()
            .groups
            .iter()
            .any(|g| g.split == Split::Evaluation)
    );
    for source in &b.inputs().sources {
        assert_eq!(
            sha256(b.source_bytes(&source.source_id).unwrap()),
            source.content_hash
        );
    }
}
// AC-0271: ten independent author-declared negative mutations are rejected;
// rehash outer documents to exercise semantic rejection instead of stale refs.
#[test]
fn fixture_independent_negative_mutations_reject_rehashed_contradictions() {
    for mutation in 0..10 {
        let dir = copy_authored();
        let (i0, l0, b0) = documents(dir.path());
        let (mut i, mut l, mut b) = (i0, l0, b0);
        match mutation {
            0 => {
                let index = l["groups"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .position(|g| g["group_id"] == "g-billing")
                    .unwrap();
                let mut group = l["groups"][index].clone();
                l["groups"][index]["case_ids"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|c| c != "c-call-unique-r2");
                let policy: PartitionPolicy =
                    serde_json::from_value(l["partition_policy"].clone()).unwrap();
                let (id, split) = (0..1000)
                    .map(|n| {
                        let id = format!("g-revision-{n}");
                        let split = validation::partition(&policy, &id);
                        (id, split)
                    })
                    .find(|(_, s)| *s == Split::Calibration)
                    .unwrap();
                group["group_id"] = json!(id);
                group["case_ids"] = json!(["c-call-unique-r2"]);
                group["split"] = serde_json::to_value(split).unwrap();
                l["groups"].as_array_mut().unwrap().push(group);
                l["groups"]
                    .as_array_mut()
                    .unwrap()
                    .sort_by(|a, b| a["group_id"].as_str().cmp(&b["group_id"].as_str()));
            }
            1 => {
                let split = l["groups"][0]["split"].as_str().unwrap();
                l["groups"][0]["split"] = json!(if split == "evaluation" {
                    "calibration"
                } else {
                    "evaluation"
                });
            }
            2 => {
                let c = i["cases"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|c| c["case_id"] == "c-channel-sqs")
                    .unwrap();
                c["candidates"][0]["family"]["subtype"] = serde_json::Value::Null;
                c["candidate_set_hash"] = json!(sha256(&canonical_json(&c["candidates"]).unwrap()));
            }
            3 => {
                let label = b["labels"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|c| c["case_id"] == "c-channel-sqs")
                    .unwrap();
                let id = label["truth"]["target_ids"][0].clone();
                label["hard_negative_target_ids"]
                    .as_array_mut()
                    .unwrap()
                    .push(id);
                label["hard_negative_target_ids"]
                    .as_array_mut()
                    .unwrap()
                    .sort_by(|a, b| a.as_str().cmp(&b.as_str()));
            }
            4 => {
                let label = b["labels"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|c| c["case_id"] == "c-call-unknown")
                    .unwrap();
                label["origin"]["exclusivity_basis"] = json!("Invented certainty");
            }
            5 => {
                let path = dir.path().join("sources/s-email-send.txt");
                let mut bytes = std::fs::read(&path).unwrap();
                bytes[0] ^= 1;
                std::fs::write(path, bytes).unwrap();
            }
            6 => {
                let c = i["cases"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|c| c["case_id"] == "c-call-unique")
                    .unwrap();
                c["candidates"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|c| c["node_id"] != "fn:billing/refund.ts#refundCard");
            }
            7 => {}
            8 => i["sources"][0]["bytes_path"] = json!("../sources/x.txt"),
            9 => {
                b["labels"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|c| c["case_id"] != "c-call-empty-universe");
            }
            _ => unreachable!(),
        }
        write_documents(dir.path(), &i, &mut l, &mut b);
        if mutation == 7 {
            let path = dir.path().join("labels.json");
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.splice(1..1, br#""kind":"labels","#.iter().copied());
            std::fs::write(path, bytes).unwrap();
        }
        assert!(
            load_fixture_bundle(dir.path()).is_err(),
            "mutation {mutation} must fail closed"
        );
    }
}

// AC-0271: visible fixtures never enter the independent loader, and mixed
// purpose documents fail even when every dependent hash has been recomputed.
#[test]
fn fixture_expected_purpose_is_exact_and_never_upgrades_visible_fixtures() {
    let dir = copy_authored();
    assert!(load_independent_dataset(dir.path()).is_err());
    let (mut i, mut l, mut b) = documents(dir.path());
    for doc in [&mut i, &mut l, &mut b] {
        doc["purpose"] = json!("independent_evaluation");
    }
    write_documents(dir.path(), &i, &mut l, &mut b);
    assert!(
        load_independent_dataset(dir.path()).is_err(),
        "independent purpose cannot retain fixture leakage rule"
    );
    catalogue_test_documents(&mut i, &mut l, &mut b);
    write_documents(dir.path(), &i, &mut l, &mut b);
    assert!(load_fixture_bundle(dir.path()).is_err());
    assert_eq!(
        load_independent_dataset(dir.path()).unwrap().purpose(),
        "independent_evaluation"
    );
    b["purpose"] = json!("visible_mechanism_fixture");
    write_documents(dir.path(), &i, &mut l, &mut b);
    assert!(load_independent_dataset(dir.path()).is_err());
    assert!(load_fixture_bundle(dir.path()).is_err());
}

fn full_catalogue_documents() -> (serde_json::Value, serde_json::Value, serde_json::Value) {
    let (mut i, mut l, mut b) = documents(&authored_root());
    let keep = ["c-call-no-match", "c-call-same-name", "c-call-unique"];
    i["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|c| keep.contains(&c["case_id"].as_str().unwrap()));
    b["labels"]
        .as_array_mut()
        .unwrap()
        .retain(|c| keep.contains(&c["case_id"].as_str().unwrap()));
    let mut catalogue = std::collections::BTreeMap::new();
    for case in i["cases"].as_array().unwrap() {
        for c in case["candidates"].as_array().unwrap() {
            catalogue.insert(c["node_id"].as_str().unwrap().to_owned(), c.clone());
        }
    }
    let candidates: Vec<_> = catalogue.into_values().collect();
    let ch = sha256(&canonical_json(&candidates).unwrap());
    for c in i["cases"].as_array_mut().unwrap() {
        c["candidates"] = json!(candidates);
        c["candidate_set_hash"] = json!(ch);
    }
    l["groups"]
        .as_array_mut()
        .unwrap()
        .retain(|g| ["g-notify", "g-billing"].contains(&g["group_id"].as_str().unwrap()));
    for g in l["groups"].as_array_mut().unwrap() {
        g["case_ids"]
            .as_array_mut()
            .unwrap()
            .retain(|c| keep.contains(&c.as_str().unwrap()));
    }
    catalogue_test_documents(&mut i, &mut l, &mut b);
    (i, l, b)
}
// AC-0271/0273: the independently-versioned rule permits a FULL shared unlabeled
// retrieval catalogue while preserving the author's original group/salt/splits.
#[test]
fn independent_catalogue_rule_allows_shared_unlabelled_occurrences_only() {
    let dir = copy_authored();
    let (mut i, mut l, mut b) = full_catalogue_documents();
    write_documents(dir.path(), &i, &mut l, &mut b);
    let bundle = load_independent_dataset(dir.path()).unwrap();
    assert!(
        bundle
            .leakage()
            .groups
            .iter()
            .any(|g| g.split == Split::Calibration)
    );
    assert!(
        bundle
            .leakage()
            .groups
            .iter()
            .any(|g| g.split == Split::Evaluation)
    );
    assert!(
        bundle
            .inputs()
            .cases
            .windows(2)
            .all(|w| w[0].candidates == w[1].candidates)
    );
    assert_eq!(bundle.inputs().cases[0].candidates.len(), 5);
    // The same rule cannot be supplied to the frozen public visible loader.
    for doc in [&mut i, &mut l, &mut b] {
        doc["purpose"] = json!("visible_mechanism_fixture");
    }
    write_documents(dir.path(), &i, &mut l, &mut b);
    assert!(load_fixture_bundle(dir.path()).is_err());
}
// AC-0271/0273: identities, sources and evidence shared as SUPERVISED
// occurrences still reject, even when all hashes and related-source unions agree.
#[test]
fn independent_catalogue_rule_rejects_supervised_positive_hardnegative_and_evidence_leaks() {
    for mutation in 0..5 {
        let dir = copy_authored();
        let (mut i, mut l, mut b) = full_catalogue_documents();
        let target = "fn:channels/email.ts#send";
        let eval_label = b["labels"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|c| c["case_id"] == "c-call-unique")
            .unwrap();
        match mutation {
            0 => eval_label["truth"]["target_ids"] = json!([target]),
            1 => eval_label["hard_negative_target_ids"] = json!([target]),
            2 => {
                eval_label["origin"]["source_ids"] =
                    json!(["s-billing-charge", "s-checkout-caller", "s-email-send"])
            }
            3 => {
                let query = i["cases"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|c| c["case_id"] == "c-call-same-name")
                    .unwrap()["hop"]["query"]
                    .clone();
                let c = i["cases"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|c| c["case_id"] == "c-call-unique")
                    .unwrap();
                c["hop"]["query"] = query;
                c["hop"]["query_hash"] =
                    json!(sha256(c["hop"]["query"].as_str().unwrap().as_bytes()));
            }
            4 => {
                let c = i["cases"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|c| c["case_id"] == "c-call-unique")
                    .unwrap();
                c["source_ids"] = json!(["s-notifier-caller"]);
                let ev = i["sources"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|s| s["source_id"] == "s-notifier-caller")
                    .unwrap()["evidence"]
                    .clone();
                let c = i["cases"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|c| c["case_id"] == "c-call-unique")
                    .unwrap();
                c["hop"]["evidence"] = json!([ev]);
            }
            _ => unreachable!(),
        }
        catalogue_test_documents(&mut i, &mut l, &mut b);
        write_documents(dir.path(), &i, &mut l, &mut b);
        assert!(
            load_independent_dataset(dir.path()).is_err(),
            "supervised leakage mutation {mutation}"
        );
    }
}
