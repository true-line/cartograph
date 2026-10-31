//! Explicit ownership of typed `from_value` decodes (#1509), replacing the
//! per-input-byte `json_bytes` currency for the provenance and omission DTOs.
//!
//! Every heap owner in the audited closure is a string payload copied or moved
//! from one distinct JSON string token (at most the input wire bytes), or one
//! collection slot per admitted array item (one counted value). Enums and
//! nested structs are inline in their parent's slot, which callers already
//! charge, so no value is credited to both a parent and a descendant. For
//! provenance and omissions, the guards below make an added field or variant,
//! or a changed field type, a compile error next to these element lists. The
//! source-rule closure is source-audited against the element list in
//! `core_graph::rules::workspace`, without a guard. Neither sees serde
//! attribute changes (flatten, untagged, an allocating `deserialize_with`);
//! that residual needs manual review, and tests check complete owned shapes
//! against the bound.
use super::{json, sum};
use core_graph::omission::{FillSeam, Omission, OmissionClass};
use core_prov::{
    ArtifactLocator, ArtifactRef, ConfidenceTier, EvidenceBasis, EvidenceRef, ProducingMethod,
    Provenance, Tier,
};
use std::mem::size_of;

/// Largest provenance collection slot.
fn provenance_element() -> usize {
    size_of::<EvidenceRef>().max(size_of::<ArtifactRef>())
}

/// Fixed strings of the explicit-Gap fallback provenance, which no input
/// token backs. Measured from the producer rather than restated.
pub(crate) fn fallback_provenance_heap() -> usize {
    let fallback = crate::workbench::fallback_provenance("");
    fallback.extractor_id.len() + fallback.content_hash.len()
}

/// Retained heap of decoded or fallback provenance for `rows` facts whose
/// `prov` subtrees hold at most `bytes` and `values` in total.
pub(crate) fn provenance_heap(bytes: usize, values: usize, rows: usize) -> Option<usize> {
    sum(&[
        bytes,
        values.checked_mul(provenance_element())?,
        rows.checked_mul(fallback_provenance_heap())?,
    ])
}

/// One current provenance decode: its owned DTO heap, its fallback, and serde's
/// buffering of one internally tagged subtree (`method` and `basis` are
/// siblings, buffered one after the other over disjoint values).
pub(crate) fn provenance_decode(bytes: usize, values: usize) -> Option<usize> {
    sum(&[provenance_heap(bytes, values, 1)?, json(0, values)?])
}

#[allow(dead_code, reason = "compile-time closure guard; never called")]
fn provenance_closure(p: &Provenance) {
    let Provenance {
        tier,
        confidence_tier,
        evidence,
        extractor_id,
        content_hash,
        method,
        basis,
    } = p;
    let _: (&Tier, &ConfidenceTier) = (tier, confidence_tier);
    let _: &Vec<EvidenceRef> = evidence;
    let _: (&String, &String) = (extractor_id, content_hash);
    evidence_closure(&evidence[0]);
    match method {
        ProducingMethod::Rule { rule, version } => {
            let _: (&String, &String) = (rule, version);
        }
        ProducingMethod::Model { model } => {
            let _: &String = model;
        }
        ProducingMethod::Human { actor } => {
            let _: &String = actor;
        }
        ProducingMethod::Unrecorded => {}
    }
    match basis {
        None | Some(EvidenceBasis::Source | EvidenceBasis::Observation) => {}
        Some(EvidenceBasis::Artifact { refs }) => {
            let _: &Vec<ArtifactRef> = refs;
            let ArtifactRef { digest, kind, at } = &refs[0];
            let _: (&String, &String) = (digest, kind);
            match at {
                ArtifactLocator::Offset { start, end } => {
                    let _: (&u64, &u64) = (start, end);
                }
                ArtifactLocator::Member { path } => {
                    let _: &String = path;
                }
            }
        }
    }
}

#[allow(dead_code, reason = "compile-time closure guard; never called")]
fn evidence_closure(e: &EvidenceRef) {
    let EvidenceRef {
        repo,
        path,
        byte_start,
        byte_end,
        commit_sha,
    } = e;
    let _: (&String, &String, &String) = (repo, path, commit_sha);
    let _: (&u64, &u64) = (byte_start, byte_end);
}

#[allow(dead_code, reason = "compile-time closure guard; never called")]
fn omission_closure(o: &Omission) {
    let Omission {
        class,
        seam,
        detail,
    } = o;
    let _: (&OmissionClass, &FillSeam, &String) = (class, seam, detail);
}

/// Unit-only enums own nothing; a data-carrying variant must fail here.
#[allow(dead_code, reason = "compile-time closure guard; never called")]
fn unit_closure(tier: Tier, confidence: ConfidenceTier, class: OmissionClass, seam: FillSeam) {
    match tier {
        Tier::Deterministic | Tier::Dynamic | Tier::Semantic | Tier::Agentic => {}
    }
    match confidence {
        ConfidenceTier::Confirmed
        | ConfidenceTier::InferredStrong
        | ConfidenceTier::InferredWeak
        | ConfidenceTier::Gap => {}
    }
    match class {
        OmissionClass::UnsupportedExtractor
        | OmissionClass::MissingInput
        | OmissionClass::UnresolvedRelationship
        | OmissionClass::ContradictoryOrStale
        | OmissionClass::ProductGap => {}
    }
    match seam {
        FillSeam::Adapter
        | FillSeam::Registry
        | FillSeam::ManifestInput
        | FillSeam::Interpretation
        | FillSeam::Requirement => {}
    }
}

/// Fixed omission detail text that no input token backs. Input-derived
/// details (`omission.detail`, `t1_reason`, `reason`, node names and edge
/// labels in fallbacks) take the byte credit instead.
const FIXED_GAP_DETAILS: &[&str] = &[
    core_graph::omission::DYNAMIC_DISPATCH,
    crate::rules::RULE_PAYLOAD_UNAVAILABLE,
    crate::rules::RULE_RELATIONSHIP_UNRESOLVED,
    crate::workbench::render::TOOLCHAIN_UNRESOLVED,
    "unresolved  edge", // format!("unresolved {} edge") around an input label
    "unresolved  hop",  // format!("unresolved {} hop") around an input label
];

fn fixed_gap_detail() -> Option<usize> {
    FIXED_GAP_DETAILS.iter().map(|text| text.len()).max()
}

/// Retained omission details for `rows` facts. Each classified reason is
/// input-derived text (within `bytes` in total) plus at most one fixed
/// fallback text, and classification adds at most its fixed prefix or
/// blank-reason text, so `max(a + b, n) <= a + max(b, n)` per fact.
pub(crate) fn omission_heap(bytes: usize, rows: usize) -> Option<usize> {
    let fixed = core_graph::omission::classified_detail_bytes(fixed_gap_detail()?)?;
    bytes.checked_add(rows.checked_mul(fixed)?)
}

/// One current omission: the caller's owned fallback reason, the classifier's
/// temporaries over it, and the detail it returns.
pub(crate) fn omission_current(bytes: usize) -> Option<usize> {
    let reason = bytes.checked_add(fixed_gap_detail()?)?;
    sum(&[
        reason,
        core_graph::omission::classification_workspace_bytes(reason)?,
        core_graph::omission::classified_detail_bytes(reason)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_prov::ArtifactLocator::{Member, Offset};

    /// Graph-convention values: every scalar and container, never a key.
    fn values(value: &serde_json::Value) -> usize {
        1 + match value {
            serde_json::Value::Array(items) => items.iter().map(values).sum::<usize>(),
            serde_json::Value::Object(fields) => fields.values().map(values).sum::<usize>(),
            _ => 0,
        }
    }

    /// Owned string payloads plus collection slots of one decoded provenance.
    fn owned(p: &Provenance) -> usize {
        let evidence: usize = p
            .evidence
            .iter()
            .map(|e| size_of::<EvidenceRef>() + e.repo.len() + e.path.len() + e.commit_sha.len())
            .sum();
        let method = match &p.method {
            ProducingMethod::Rule { rule, version } => rule.len() + version.len(),
            ProducingMethod::Model { model } => model.len(),
            ProducingMethod::Human { actor } => actor.len(),
            ProducingMethod::Unrecorded => 0,
        };
        let basis = match &p.basis {
            Some(EvidenceBasis::Artifact { refs }) => refs
                .iter()
                .map(|r| {
                    size_of::<ArtifactRef>()
                        + r.digest.len()
                        + r.kind.len()
                        + match &r.at {
                            Member { path } => path.len(),
                            Offset { .. } => 0,
                        }
                })
                .sum(),
            _ => 0,
        };
        evidence + p.extractor_id.len() + p.content_hash.len() + method + basis
    }

    fn evidence(n: usize) -> Vec<EvidenceRef> {
        (0..n)
            .map(|i| EvidenceRef {
                repo: "r".into(),
                path: "é\"\n".repeat(i % 3),
                byte_start: 0,
                byte_end: 1,
                commit_sha: String::new(),
            })
            .collect()
    }

    #[test]
    fn typed_provenance_heap_covers_complete_decoded_shapes() {
        // AC-0522: every method and basis variant, empty and concentrated
        // collections; the decoded owner never exceeds its typed credit.
        let artifact = |n: usize| EvidenceBasis::Artifact {
            refs: (0..n)
                .map(|i| ArtifactRef {
                    digest: format!("sha256:{}", "a".repeat(64)),
                    kind: "jvm-class".into(),
                    at: if i % 2 == 0 {
                        Offset { start: 0, end: 1 }
                    } else {
                        Member {
                            path: "a/b.class".into(),
                        }
                    },
                })
                .collect(),
        };
        let methods = [
            ProducingMethod::Unrecorded,
            ProducingMethod::Rule {
                rule: "r".into(),
                version: "1".into(),
            },
            ProducingMethod::Model { model: "m".into() },
            ProducingMethod::Human { actor: "h".into() },
        ];
        let bases = [
            None,
            Some(EvidenceBasis::Source),
            Some(EvidenceBasis::Observation),
            Some(artifact(1)),
            Some(artifact(64)),
        ];
        for method in &methods {
            for basis in &bases {
                for n in [0, 1, 200] {
                    let mut p = Provenance::new(
                        Tier::Deterministic,
                        ConfidenceTier::Confirmed,
                        evidence(n),
                        "x",
                        b"fact",
                    )
                    .unwrap();
                    p.method = method.clone();
                    p.basis = basis.clone();
                    let value = serde_json::to_value(&p).unwrap();
                    let wire = serde_json::to_vec(&value).unwrap().len();
                    let count = values(&value);
                    let decoded: Provenance = serde_json::from_value(value).unwrap();
                    assert!(owned(&decoded) <= provenance_heap(wire, count, 0).unwrap());
                    assert!(
                        provenance_decode(wire, count).unwrap()
                            >= provenance_heap(wire, count, 1).unwrap() + json(0, count).unwrap()
                    );
                }
            }
        }
        let fallback = crate::workbench::fallback_provenance("any identity");
        assert_eq!(owned(&fallback), fallback_provenance_heap());
    }

    #[test]
    fn typed_omission_heap_covers_fixed_and_classified_details() {
        // AC-0522: classification adds at most its fixed prefix or blank
        // text; fixed fallbacks are charged per fact, input text by bytes.
        let fixed = fixed_gap_detail().unwrap();
        for reason in FIXED_GAP_DETAILS
            .iter()
            .map(|text| text.to_string())
            .chain([
                "".into(),
                " ".into(),
                "zz qq".into(),
                "unresolved X edge".into(),
            ])
        {
            let detail = Omission::classify(&reason).detail;
            assert!(detail.len() <= omission_heap(reason.len(), 1).unwrap());
            assert!(detail.len() <= omission_current(reason.len()).unwrap());
        }
        assert!(
            omission_heap(0, 1).unwrap() >= fixed + core_graph::omission::UNCLASSIFIED_PREFIX.len()
        );
    }
}
