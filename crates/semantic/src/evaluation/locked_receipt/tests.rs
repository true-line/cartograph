// AC-1511: the host admits only on a receipt verified against its pinned key
// and revalidated against its own parameters, embedding space and corpora.
use super::*;
use crate::{
    HopKind,
    embedding_cache::EmbeddingCache,
    evaluation::{
        fixtures,
        locked::{self, ReviewRecord},
        policy::planned_embedding_texts,
        policy::{CalibrationView, EvaluationInputs},
        report::{DecisionReason, FamilyStatus},
        reviewed::tests::{Scripted, independent_files, live},
    },
};
use llm::LlmProvider;
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use std::{
    path::Path,
    sync::{Arc, Mutex, atomic::AtomicUsize},
    time::{SystemTime, UNIX_EPOCH},
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn key() -> Ed25519KeyPair {
    let doc = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    Ed25519KeyPair::from_pkcs8(doc.as_ref()).unwrap()
}

fn sign(key: &Ed25519KeyPair, receipt: &[u8]) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "receipt": std::str::from_utf8(receipt).unwrap(),
        "signature": hex(key.sign(receipt).as_ref()),
        "public_key": hex(key.public_key().as_ref()),
    }))
    .unwrap()
}

fn provider() -> Scripted {
    Scripted {
        calls: AtomicUsize::new(0),
        version: AtomicUsize::new(1),
    }
}

fn parameters(bundle: &fixtures::LoadedFixtureBundle, threshold: f32) -> Vec<FamilyPolicy> {
    bundle
        .inputs()
        .cases
        .iter()
        .map(|c| c.family.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|family| FamilyPolicy {
            family,
            threshold,
            ambiguity_margin: 0.01,
            neighbors: 3,
            min_evaluation_cases: 1,
            min_answerable_cases: 1,
            min_no_match_cases: 1,
            precision_floor: 0.95,
        })
        .collect()
}

fn request(bundle: &fixtures::LoadedFixtureBundle, threshold: f32) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "families": parameters(bundle, threshold).iter().map(|p| serde_json::json!({
            "family": p.family, "threshold": p.threshold, "ambiguity_margin": p.ambiguity_margin,
            "neighbors": p.neighbors, "min_evaluation_cases": p.min_evaluation_cases,
            "min_answerable_cases": p.min_answerable_cases,
            "min_no_match_cases": p.min_no_match_cases, "precision_floor": p.precision_floor,
        })).collect::<Vec<_>>(),
    }))
    .unwrap()
}

/// A locked store scored once: the runner's receipt bytes, the store and the
/// dataset copy the host observes as its own selected corpora.
fn scored() -> (tempfile::TempDir, Vec<u8>, fixtures::LoadedFixtureBundle) {
    let dataset = independent_files(false);
    let store = tempfile::tempdir().unwrap();
    assert!(
        std::process::Command::new("cp")
            .arg("-R")
            .arg(dataset.path())
            .arg(store.path().join("dataset"))
            .status()
            .unwrap()
            .success()
    );
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let review = ReviewRecord {
        schema_version: 1,
        reviewer: "separate-reviewer".into(),
        policy_implementer: "policy-implementer".into(),
        reviewed_at: now - 1000,
    };
    std::fs::write(
        store.path().join("review.json"),
        serde_json::to_vec(&review).unwrap(),
    )
    .unwrap();
    let bundle = fixtures::load_independent_dataset(&store.path().join("dataset")).unwrap();
    let receipt = locked::score(store.path(), &request(&bundle, 0.9), &provider()).unwrap();
    (store, receipt, bundle)
}

fn observed(store: &Path, bundle: &fixtures::LoadedFixtureBundle) -> Vec<ObservedCorpus> {
    locked::observed(&store.join("dataset"), bundle).unwrap()
}

/// The host's own embedding operation: a separate cache, sealed over the
/// texts it will rank.
fn host_cache<'a>(
    dir: &Path,
    provider: &'a Scripted,
    bundle: &fixtures::LoadedFixtureBundle,
) -> CachedProvider<'a> {
    let mut cache = EmbeddingCache::open(dir.join("host-cache.db")).unwrap();
    let lease = cache.bind_instance(&"b".repeat(32)).unwrap();
    let cached = CachedProvider::new(provider, Arc::new(Mutex::new(cache)), lease).unwrap();
    let texts = planned_embedding_texts(
        &CalibrationView::from_bundle(bundle),
        &EvaluationInputs::from_bundle(bundle),
    )
    .unwrap();
    cached.embed(&texts).unwrap();
    cached.seal().unwrap();
    cached
}

fn call_decision(admission: &LiveAdmission) -> (FamilyStatus, DecisionReason) {
    let call = admission
        .summary()
        .families
        .iter()
        .find(|f| f.family == FamilyKey::new(HopKind::Call, None))
        .unwrap();
    (call.status, call.reason)
}

#[test]
fn locked_receipt_verifies_only_against_the_pinned_key() {
    let (_store, receipt, _) = scored();
    let signer = key();
    let pinned = hex(signer.public_key().as_ref());
    let signed = sign(&signer, &receipt);
    assert!(VerifiedReceipt::verify(&signed, &pinned).is_ok());

    // Another signer, even one naming itself in the document, is refused.
    let other = key();
    let forged = sign(&other, &receipt);
    assert_eq!(
        VerifiedReceipt::verify(&forged, &pinned).unwrap_err(),
        ReceiptError::BadSignature
    );
    assert_eq!(
        VerifiedReceipt::verify(&signed, &hex(other.public_key().as_ref())).unwrap_err(),
        ReceiptError::BadSignature
    );
    // A changed receipt fails the signature.
    let mut value: serde_json::Value = serde_json::from_slice(&signed).unwrap();
    let text = value["receipt"]
        .as_str()
        .unwrap()
        .replace("\"passed\":false", "\"passed\":true");
    assert_ne!(text, value["receipt"].as_str().unwrap());
    value["receipt"] = text.into();
    let tampered = serde_json::to_vec(&value).unwrap();
    assert!(VerifiedReceipt::verify(&tampered, &pinned).is_err());
    // A malformed pinned key, an unknown field or an oversized document is refused.
    assert_eq!(
        VerifiedReceipt::verify(&signed, "zz").unwrap_err(),
        ReceiptError::InvalidPinnedKey
    );
    let mut extra: serde_json::Value = serde_json::from_slice(&signed).unwrap();
    extra["note"] = "x".into();
    assert_eq!(
        VerifiedReceipt::verify(&serde_json::to_vec(&extra).unwrap(), &pinned).unwrap_err(),
        ReceiptError::Malformed
    );
    assert_eq!(
        VerifiedReceipt::verify(&vec![b' '; MAX_SIGNED_BYTES + 1], &pinned).unwrap_err(),
        ReceiptError::Malformed
    );
    // A validly signed document of another purpose is refused after the signature.
    let mut other_purpose: serde_json::Value = serde_json::from_slice(&receipt).unwrap();
    other_purpose["purpose"] = "reviewed_independent_dataset".into();
    let bytes = serde_json::to_vec(&other_purpose).unwrap();
    assert_eq!(
        VerifiedReceipt::verify(&sign(&signer, &bytes), &pinned).unwrap_err(),
        ReceiptError::Malformed
    );
}

#[test]
fn locked_receipt_admits_on_matching_live_state_and_abstains_otherwise() {
    let (store, receipt, bundle) = scored();
    let signer = key();
    let pinned = hex(signer.public_key().as_ref());
    let verified = VerifiedReceipt::verify(&sign(&signer, &receipt), &pinned).unwrap();
    let observed = observed(store.path(), &bundle);
    let host = tempfile::tempdir().unwrap();
    let model = provider();
    let cached = host_cache(host.path(), &model, &bundle);
    let (hops, candidates) = live(&bundle, "c-call-unique");

    // The host's own parameters, space and corpora match: the call family admits.
    let ok = verified.admit_live(
        parameters(&bundle, 0.9),
        &observed,
        &cached,
        &hops,
        &candidates,
    );
    assert_eq!(ok.admitted().len(), 1);
    assert!(!ok.staged().is_empty());
    assert_eq!(
        call_decision(&ok),
        (
            FamilyStatus::IndependentlyEvaluated,
            DecisionReason::IndependentlyEvaluated
        )
    );
    let decision = &ok.summary().families[0];
    assert_eq!(
        decision.receipt_digest.as_deref(),
        Some(verified.receipt_digest())
    );

    // Different parameters reproduce a different policy digest.
    let params = verified.admit_live(
        parameters(&bundle, 0.8),
        &observed,
        &cached,
        &hops,
        &candidates,
    );
    // A changed corpus observation.
    let mut moved = observed.clone();
    moved[0].revision = "other-revision".into();
    let corpora = verified.admit_live(
        parameters(&bundle, 0.9),
        &moved,
        &cached,
        &hops,
        &candidates,
    );
    // A different embedding space: another model version.
    let other_model = Scripted {
        calls: AtomicUsize::new(0),
        version: AtomicUsize::new(2),
    };
    let other_host = tempfile::tempdir().unwrap();
    let other_space = host_cache(other_host.path(), &other_model, &bundle);
    let space = verified.admit_live(
        parameters(&bundle, 0.9),
        &observed,
        &other_space,
        &hops,
        &candidates,
    );
    // An invalid parameter set cannot be frozen at all.
    let invalid = verified.admit_live(Vec::new(), &observed, &cached, &hops, &candidates);
    for refused in [&params, &corpora, &space, &invalid] {
        assert!(refused.admitted().is_empty());
        assert_eq!(
            call_decision(refused),
            (
                FamilyStatus::Abstained,
                DecisionReason::IncompatibleEvidence
            )
        );
    }
}
