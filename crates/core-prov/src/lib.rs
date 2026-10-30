//! Provenance and confidence model — the integrity spine (SPEC-00 §2, §4.3).
//!
//! Every node/edge in the graph carries a [`Provenance`]: which tier produced
//! it, its confidence tier, the evidence spans behind it, and a content hash
//! that makes re-ingest idempotent (US-0007, US-0014).

use serde::{Deserialize, Serialize};

/// Producing tier of a fact — the escalation ladder, lowest wins (SPEC-00 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tier {
    /// T0 — static parse, IaC graph, framework adapters.
    Deterministic,
    /// T1 — execution-derived evidence (state exports, traces, test runs).
    Dynamic,
    /// T2 — embeddings/similarity/name-contract matching.
    Semantic,
    /// T3 — bounded LLM agent proposals with cited evidence.
    Agentic,
}

/// Confidence tier carried by every fact (SPEC-00 §13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfidenceTier {
    /// Established deterministically or by observation (T0/T1).
    Confirmed,
    /// Inferred by the semantic tier (T2).
    InferredStrong,
    /// Proposed by the agentic tier (T3).
    InferredWeak,
    /// Explicitly unresolved — preferred over an unsupported assertion.
    Gap,
}

impl Tier {
    /// The highest confidence a producer at this tier may assert (SPEC-00 §2).
    pub fn confidence_ceiling(self) -> ConfidenceTier {
        match self {
            Tier::Deterministic | Tier::Dynamic => ConfidenceTier::Confirmed,
            Tier::Semantic => ConfidenceTier::InferredStrong,
            Tier::Agentic => ConfidenceTier::InferredWeak,
        }
    }
}

/// R-INT-1: may `producer` write over a fact currently held at `existing`?
///
/// T2/T3 never overwrite or upgrade a T0/T1 (`Confirmed`) fact — they only
/// fill unresolved slots (`Gap`) or revise their own tier's output.
pub fn may_overwrite(producer: Tier, existing: ConfidenceTier) -> bool {
    match producer {
        Tier::Deterministic | Tier::Dynamic => true,
        Tier::Semantic => matches!(
            existing,
            ConfidenceTier::InferredStrong | ConfidenceTier::InferredWeak | ConfidenceTier::Gap
        ),
        Tier::Agentic => matches!(existing, ConfidenceTier::InferredWeak | ConfidenceTier::Gap),
    }
}

/// How a fact was produced, recorded separately from its claim kind, tier
/// and confidence (ADR-0036 §3, AC-0244). It labels derived statements
/// without a new confidence label and never changes tier or confidence:
/// accepting or validating a model's output keeps it a model's output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProducingMethod {
    /// A named, versioned deterministic rule.
    Rule {
        /// Rule name (e.g. `projection.api-catalog`).
        rule: String,
        /// Rule version; a changed rule is a different producer.
        version: String,
    },
    /// A model: an embedding model or an LLM, named by its provider id.
    Model {
        /// Provider/model id as the producer reported it.
        model: String,
    },
    /// A human decision.
    Human {
        /// Who decided, as recorded by the deciding surface.
        actor: String,
    },
    /// Legacy facts written before methods were recorded, and producers
    /// that do not record one yet. Shown explicitly as "not recorded".
    #[default]
    Unrecorded,
}

impl ProducingMethod {
    /// Whether no method was recorded.
    pub fn is_unrecorded(&self) -> bool {
        matches!(self, ProducingMethod::Unrecorded)
    }

    /// Short human label: `rule name@version`, `model id`, `human actor`,
    /// or `not recorded`.
    pub fn label(&self) -> String {
        match self {
            ProducingMethod::Rule { rule, version } => format!("rule {rule}@{version}"),
            ProducingMethod::Model { model } => format!("model {model}"),
            ProducingMethod::Human { actor } => format!("human {actor}"),
            ProducingMethod::Unrecorded => "not recorded".into(),
        }
    }
}

/// A source span backing a fact (SPEC-00 §4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// Repository the evidence lives in (e.g. `owner/name`).
    pub repo: String,
    /// Path within the repository.
    pub path: String,
    /// Byte span start (inclusive).
    pub byte_start: u64,
    /// Byte span end (exclusive).
    pub byte_end: u64,
    /// Commit the span was read at.
    pub commit_sha: String,
}

/// What kind of evidence a fact rests on, recorded separately from tier
/// (ADR-0051 decision 5). A static read of an artifact is T0 like a source
/// parse, but its evidence is the artifact, not a source span.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceBasis {
    /// Source text, cited by [`EvidenceRef`] spans.
    Source,
    /// A built artifact (bytecode, bundle, binary, image), cited by digest.
    Artifact {
        /// The artifact evidence; at least one.
        refs: Vec<ArtifactRef>,
    },
    /// Runtime or execution observation (T1).
    Observation,
}

impl EvidenceBasis {
    /// An artifact basis needs at least one valid [`ArtifactRef`].
    pub fn validate(&self) -> Result<(), IntegrityError> {
        match self {
            EvidenceBasis::Artifact { refs } if refs.is_empty() => {
                Err(IntegrityError::Artifact("no artifact evidence"))
            }
            EvidenceBasis::Artifact { refs } => refs.iter().try_for_each(ArtifactRef::validate),
            EvidenceBasis::Source | EvidenceBasis::Observation => Ok(()),
        }
    }
}

/// Where in an artifact the evidence is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactLocator {
    /// A byte span of the artifact, end exclusive.
    Offset {
        /// Byte span start (inclusive).
        start: u64,
        /// Byte span end (exclusive).
        end: u64,
    },
    /// A member of an archive or image, by relative path.
    Member {
        /// Relative member path (e.g. `com/acme/Pay.class`).
        path: String,
    },
}

/// Artifact evidence: the artifact's content digest, its kind and where in
/// it the fact was read (ADR-0051 decision 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// `sha256:` and 64 lowercase hex digits of the artifact bytes.
    pub digest: String,
    /// Artifact kind named by its reader (e.g. `jvm-class`, `js-bundle`):
    /// 1 to 64 lowercase ASCII letters, digits and hyphens.
    pub kind: String,
    /// Offset or member path.
    pub at: ArtifactLocator,
}

/// Longest artifact member path accepted.
pub const MAX_MEMBER_PATH_BYTES: usize = 4096;

impl ArtifactRef {
    /// Check digest, kind and locator shape.
    pub fn validate(&self) -> Result<(), IntegrityError> {
        let hex = self.digest.strip_prefix("sha256:").unwrap_or_default();
        if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(IntegrityError::Artifact("digest"));
        }
        if self.kind.is_empty()
            || self.kind.len() > 64
            || !self
                .kind
                .bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
        {
            return Err(IntegrityError::Artifact("kind"));
        }
        match &self.at {
            ArtifactLocator::Offset { start, end } if start < end => Ok(()),
            ArtifactLocator::Offset { .. } => Err(IntegrityError::Artifact("offset")),
            ArtifactLocator::Member { path }
                if !path.is_empty()
                    && path.len() <= MAX_MEMBER_PATH_BYTES
                    && !path.contains(['\\', '\0'])
                    // A colon in the first segment is a drive (`C:/`, `C:x`)
                    // or a scheme on some platform, never a member.
                    && !path.split('/').next().unwrap_or_default().contains(':')
                    && path.split('/').all(|seg| !matches!(seg, "" | "." | "..")) =>
            {
                Ok(())
            }
            ArtifactLocator::Member { .. } => Err(IntegrityError::Artifact("member path")),
        }
    }
}

/// Provenance attached to every node and edge (SPEC-00 §4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// Producing tier.
    pub tier: Tier,
    /// Confidence tier (never above `tier.confidence_ceiling()`).
    pub confidence_tier: ConfidenceTier,
    /// Evidence spans (0..n).
    pub evidence: Vec<EvidenceRef>,
    /// Which adapter/resolver produced the fact.
    pub extractor_id: String,
    /// Content-addressed hash for idempotent re-ingest. Derived only from the
    /// fact's canonical bytes — never timestamps (US-0014).
    pub content_hash: String,
    /// Producing method (AC-0244). Omitted from the wire form when not
    /// recorded, so facts without one serialize exactly as before.
    #[serde(default, skip_serializing_if = "ProducingMethod::is_unrecorded")]
    pub method: ProducingMethod,
    /// Evidence basis (ADR-0051 decision 5). `None` is the default basis
    /// for the tier, see [`Provenance::basis`], and is omitted from the wire
    /// form so existing facts serialize exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<EvidenceBasis>,
}

impl Provenance {
    /// Build provenance for a fact, deriving the content hash from the fact's
    /// canonical byte representation. Enforces the tier's confidence ceiling.
    pub fn new(
        tier: Tier,
        confidence_tier: ConfidenceTier,
        evidence: Vec<EvidenceRef>,
        extractor_id: impl Into<String>,
        fact_bytes: &[u8],
    ) -> Result<Self, IntegrityError> {
        if confidence_above_ceiling(tier, confidence_tier) {
            return Err(IntegrityError::AboveCeiling {
                tier,
                confidence_tier,
            });
        }
        Ok(Self {
            tier,
            confidence_tier,
            evidence,
            extractor_id: extractor_id.into(),
            content_hash: content_hash(fact_bytes),
            method: ProducingMethod::Unrecorded,
            basis: None,
        })
    }

    /// Record the producing method. Tier, confidence, evidence and content
    /// hash are unchanged.
    #[must_use]
    pub fn with_method(mut self, method: ProducingMethod) -> Self {
        self.method = method;
        self
    }

    /// Record the evidence basis. Tier, confidence, evidence and content
    /// hash are unchanged.
    #[must_use]
    pub fn with_basis(mut self, basis: EvidenceBasis) -> Self {
        self.basis = Some(basis);
        self
    }

    /// The recorded basis, or the tier's default: `observation` for T1,
    /// `source` otherwise.
    pub fn basis(&self) -> EvidenceBasis {
        match (&self.basis, self.tier) {
            (Some(basis), _) => basis.clone(),
            (None, Tier::Dynamic) => EvidenceBasis::Observation,
            (None, _) => EvidenceBasis::Source,
        }
    }

    /// Re-validate provenance loaded from an external graph or IPC payload.
    ///
    /// Serde reconstructs the wire shape but cannot call [`Provenance::new`],
    /// so consumers use this before trusting the encoded confidence tier.
    pub fn validate(&self) -> Result<(), IntegrityError> {
        if confidence_above_ceiling(self.tier, self.confidence_tier) {
            return Err(IntegrityError::AboveCeiling {
                tier: self.tier,
                confidence_tier: self.confidence_tier,
            });
        }
        self.basis.as_ref().map_or(Ok(()), EvidenceBasis::validate)
    }
}

fn confidence_above_ceiling(tier: Tier, confidence: ConfidenceTier) -> bool {
    fn rank(c: ConfidenceTier) -> u8 {
        match c {
            ConfidenceTier::Confirmed => 3,
            ConfidenceTier::InferredStrong => 2,
            ConfidenceTier::InferredWeak => 1,
            ConfidenceTier::Gap => 0,
        }
    }
    rank(confidence) > rank(tier.confidence_ceiling())
}

/// Violations of the integrity rules (R-INT-1..5).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IntegrityError {
    /// A producer asserted a confidence above its tier's ceiling.
    #[error("{tier:?} may not assert {confidence_tier:?} (above ceiling)")]
    AboveCeiling {
        /// Producing tier.
        tier: Tier,
        /// Asserted confidence.
        confidence_tier: ConfidenceTier,
    },
    /// Artifact evidence is missing or malformed in the named part.
    #[error("artifact evidence: invalid {0}")]
    Artifact(&'static str),
}

/// Content-address a byte sequence (BLAKE3, hex). Deterministic by
/// construction — the foundation of idempotent re-ingest (AC-0018, AC-0039).
pub fn content_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_deterministic_and_content_sensitive() {
        assert_eq!(content_hash(b"same input"), content_hash(b"same input"));
        assert_ne!(content_hash(b"same input"), content_hash(b"same input!"));
    }

    #[test]
    fn confidence_ceilings_match_spec() {
        assert_eq!(
            Tier::Deterministic.confidence_ceiling(),
            ConfidenceTier::Confirmed
        );
        assert_eq!(
            Tier::Dynamic.confidence_ceiling(),
            ConfidenceTier::Confirmed
        );
        assert_eq!(
            Tier::Semantic.confidence_ceiling(),
            ConfidenceTier::InferredStrong
        );
        assert_eq!(
            Tier::Agentic.confidence_ceiling(),
            ConfidenceTier::InferredWeak
        );
    }

    #[test]
    fn r_int_1_t2_t3_never_touch_confirmed() {
        // AC-0019: T2/T3 cannot overwrite or upgrade a T0/T1 fact.
        assert!(!may_overwrite(Tier::Semantic, ConfidenceTier::Confirmed));
        assert!(!may_overwrite(Tier::Agentic, ConfidenceTier::Confirmed));
        // Agents also cannot touch the semantic tier's output.
        assert!(!may_overwrite(
            Tier::Agentic,
            ConfidenceTier::InferredStrong
        ));
        // Everyone may fill an explicit Gap.
        for tier in [
            Tier::Deterministic,
            Tier::Dynamic,
            Tier::Semantic,
            Tier::Agentic,
        ] {
            assert!(may_overwrite(tier, ConfidenceTier::Gap));
        }
        // T0/T1 may replace anything.
        assert!(may_overwrite(
            Tier::Deterministic,
            ConfidenceTier::Confirmed
        ));
        assert!(may_overwrite(Tier::Dynamic, ConfidenceTier::InferredStrong));
    }

    #[test]
    fn provenance_rejects_confidence_above_ceiling() {
        // An agent asserting Confirmed is an integrity violation, not a value.
        let err = Provenance::new(
            Tier::Agentic,
            ConfidenceTier::Confirmed,
            vec![],
            "t3.test",
            b"x",
        )
        .unwrap_err();
        assert!(matches!(err, IntegrityError::AboveCeiling { .. }));
    }

    #[test]
    fn provenance_serde_round_trips() {
        let p = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![EvidenceRef {
                repo: "qwtm/example".into(),
                path: "src/index.ts".into(),
                byte_start: 10,
                byte_end: 42,
                commit_sha: "abc123".into(),
            }],
            "t0.ts-adapter",
            b"endpoint GET /health",
        )
        .unwrap();
        let json = serde_json::to_string(&p).unwrap();
        let back: Provenance = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn deserialized_provenance_can_be_revalidated() {
        let forged: Provenance = serde_json::from_value(serde_json::json!({
            "tier": "Agentic",
            "confidence_tier": "Confirmed",
            "evidence": [],
            "extractor_id": "untrusted.payload",
            "content_hash": "not-trusted",
        }))
        .unwrap();
        assert!(matches!(
            forged.validate(),
            Err(IntegrityError::AboveCeiling { .. })
        ));
    }

    #[test]
    fn evidence_basis_defaults_by_tier_and_keeps_the_legacy_wire_form() {
        // AC-0502 (T-0502): facts without a basis serialize exactly as
        // before and read back as `source`, or `observation` for T1.
        let legacy = serde_json::json!({
            "tier": "Deterministic", "confidence_tier": "Confirmed", "evidence": [
                {"repo": "acme/api", "path": "a.py", "byte_start": 0, "byte_end": 4, "commit_sha": "abc"}
            ], "extractor_id": "t0.python", "content_hash": "h"
        });
        let read: Provenance = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(read.basis, None);
        assert_eq!(read.basis(), EvidenceBasis::Source);
        assert_eq!(serde_json::to_value(&read).unwrap(), legacy);
        let fresh = Provenance::new(
            Tier::Semantic,
            ConfidenceTier::InferredStrong,
            vec![],
            "t2",
            b"f",
        )
        .unwrap();
        assert!(serde_json::to_value(&fresh).unwrap().get("basis").is_none());
        for (tier, basis) in [
            (Tier::Deterministic, EvidenceBasis::Source),
            (Tier::Dynamic, EvidenceBasis::Observation),
            (Tier::Semantic, EvidenceBasis::Source),
            (Tier::Agentic, EvidenceBasis::Source),
        ] {
            let p = Provenance::new(tier, ConfidenceTier::Gap, vec![], "x", b"f").unwrap();
            assert_eq!(p.basis(), basis, "{tier:?}");
        }
    }

    fn artifact(at: ArtifactLocator) -> ArtifactRef {
        ArtifactRef {
            digest: format!("sha256:{}", "ab".repeat(32)),
            kind: "jvm-class".into(),
            at,
        }
    }

    #[test]
    fn an_artifact_basis_round_trips_beside_tier_and_confidence() {
        // AC-0502 (T-0502): the basis is its own field; recording it never
        // changes tier, confidence or content hash, and the confidence
        // ceiling (which verified-only exports filter on) still applies.
        let base = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![],
            "t0.jvm",
            b"f",
        )
        .unwrap();
        let basis = EvidenceBasis::Artifact {
            refs: vec![
                artifact(ArtifactLocator::Member {
                    path: "com/acme/Pay.class".into(),
                }),
                artifact(ArtifactLocator::Offset { start: 16, end: 64 }),
            ],
        };
        let read = base.clone().with_basis(basis.clone());
        assert_eq!(
            (read.tier, read.confidence_tier, &read.content_hash),
            (base.tier, base.confidence_tier, &base.content_hash)
        );
        assert!(read.validate().is_ok());
        let wire = serde_json::to_value(&read).unwrap();
        assert_eq!(wire["basis"]["kind"], "artifact");
        assert_eq!(
            wire["basis"]["refs"][0]["at"]["member"]["path"],
            "com/acme/Pay.class"
        );
        assert_eq!(wire["basis"]["refs"][1]["at"]["offset"]["end"], 64);
        assert_eq!(serde_json::from_value::<Provenance>(wire).unwrap(), read);
        assert_eq!(read.basis(), basis);
        let observed = base.clone().with_basis(EvidenceBasis::Observation);
        assert_eq!(
            serde_json::to_value(&observed).unwrap()["basis"],
            serde_json::json!({"kind": "observation"})
        );
        let mut above = read.clone();
        above.tier = Tier::Semantic;
        assert!(matches!(
            above.validate(),
            Err(IntegrityError::AboveCeiling { .. })
        ));
    }

    #[test]
    fn artifact_refs_validate_digest_kind_and_locator() {
        // AC-0502 (T-0502).
        let member = |path: &str| ArtifactLocator::Member { path: path.into() };
        let ok = artifact(member("a/b.class"));
        assert!(ok.validate().is_ok());
        let digests = [
            "ab".repeat(32),
            format!("sha1:{}", "ab".repeat(32)),
            format!("sha256:{}", "AB".repeat(32)),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}g", "a".repeat(63)),
        ];
        for digest in digests {
            let bad = ArtifactRef {
                digest,
                ..ok.clone()
            };
            assert_eq!(bad.validate(), Err(IntegrityError::Artifact("digest")));
        }
        for kind in [
            String::new(),
            "JVM".into(),
            "jvm_class".into(),
            "k".repeat(65),
        ] {
            let bad = ArtifactRef { kind, ..ok.clone() };
            assert_eq!(bad.validate(), Err(IntegrityError::Artifact("kind")));
        }
        assert!(
            artifact(ArtifactLocator::Offset { start: 0, end: 1 })
                .validate()
                .is_ok()
        );
        for (start, end) in [(4, 4), (5, 4)] {
            assert_eq!(
                artifact(ArtifactLocator::Offset { start, end }).validate(),
                Err(IntegrityError::Artifact("offset"))
            );
        }
        let long = "p".repeat(MAX_MEMBER_PATH_BYTES + 1);
        assert!(artifact(member(&long[1..])).validate().is_ok());
        assert!(artifact(member("META-INF/x:y")).validate().is_ok());
        for path in [
            "C:/Windows/x.dll",
            "C:x.dll",
            "c:",
            "file:a/b",
            "",
            "/abs",
            "a/../b",
            "a//b",
            "a/",
            "./a",
            "a\\b",
            "a\0b",
            long.as_str(),
        ] {
            assert_eq!(
                artifact(member(path)).validate(),
                Err(IntegrityError::Artifact("member path")),
                "{path:?}"
            );
        }
        let empty = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![],
            "x",
            b"f",
        )
        .unwrap()
        .with_basis(EvidenceBasis::Artifact { refs: vec![] });
        assert_eq!(
            empty.validate(),
            Err(IntegrityError::Artifact("no artifact evidence"))
        );
        let forged: Provenance = serde_json::from_value(serde_json::json!({
            "tier": "Deterministic", "confidence_tier": "Confirmed", "evidence": [],
            "extractor_id": "x", "content_hash": "h",
            "basis": {"kind": "artifact", "refs": [{"digest": "sha256:00", "kind": "jar", "at": {"offset": {"start": 0, "end": 1}}}]}
        }))
        .unwrap();
        assert_eq!(forged.validate(), Err(IntegrityError::Artifact("digest")));
    }

    #[test]
    fn producing_method_is_recorded_separately_from_tier_and_confidence() {
        // AC-0244 (T-0244): the method is its own field; recording it never
        // changes tier, confidence or content hash, and a fact without one
        // keeps the legacy wire form and reads as "not recorded".
        let base = Provenance::new(
            Tier::Semantic,
            ConfidenceTier::InferredWeak,
            vec![],
            "t2.rule",
            b"fact",
        )
        .unwrap();
        let legacy_wire = serde_json::to_value(&base).unwrap();
        assert!(legacy_wire.get("method").is_none());
        let legacy: Provenance = serde_json::from_value(legacy_wire).unwrap();
        assert_eq!(legacy.method, ProducingMethod::Unrecorded);
        assert_eq!(legacy.method.label(), "not recorded");

        let ruled = base.clone().with_method(ProducingMethod::Rule {
            rule: "projection.api-catalog".into(),
            version: "1".into(),
        });
        assert_eq!(ruled.tier, base.tier);
        assert_eq!(ruled.confidence_tier, base.confidence_tier);
        assert_eq!(ruled.content_hash, base.content_hash);
        let wire = serde_json::to_value(&ruled).unwrap();
        assert_eq!(
            wire["method"],
            serde_json::json!({"kind": "rule", "rule": "projection.api-catalog", "version": "1"})
        );
        assert_eq!(serde_json::from_value::<Provenance>(wire).unwrap(), ruled);
        assert_eq!(ruled.method.label(), "rule projection.api-catalog@1");

        // A fully justified structural projection under a named rule may be T0.
        let projection = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![],
            "projection",
            b"projection",
        )
        .unwrap()
        .with_method(ruled.method.clone());
        assert!(projection.validate().is_ok());
        let model = ProducingMethod::Model {
            model: "local:llm".into(),
        };
        assert_eq!(model.label(), "model local:llm");
        let human = ProducingMethod::Human {
            actor: "owner".into(),
        };
        assert_eq!(human.label(), "human owner");
    }
}
