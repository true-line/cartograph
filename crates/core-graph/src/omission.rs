//! Omission taxonomy (ADR-0036, #585, AC-0254): why a fact is missing, in a
//! way a reader can act on, and which seam would fill it.
//!
//! A producer may record an explicit `omission` on a Gap's properties. Every
//! other Gap is classified deterministically from the stop reasons it
//! already records, so classification never changes how Gaps are produced
//! or hashed. Rules are ordered and literal; a reason no rule recognizes is
//! said to be unclassified rather than guessed.

use serde::{Deserialize, Serialize};

/// Why something is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OmissionClass {
    /// No extractor supports the construct yet.
    UnsupportedExtractor,
    /// An input the analysis needs was not supplied or could not be read.
    MissingInput,
    /// Both ends may exist, but the relationship between them is not proven.
    UnresolvedRelationship,
    /// Observations disagree, are ambiguous, or are out of date.
    ContradictoryOrStale,
    /// A genuine gap in the product or its requirements, not in analysis.
    ProductGap,
}

impl OmissionClass {
    /// Every class, in display order.
    pub const ALL: [Self; 5] = [
        Self::UnsupportedExtractor,
        Self::MissingInput,
        Self::UnresolvedRelationship,
        Self::ContradictoryOrStale,
        Self::ProductGap,
    ];

    /// Reader-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::UnsupportedExtractor => "unsupported extractor",
            Self::MissingInput => "missing input",
            Self::UnresolvedRelationship => "unresolved relationship",
            Self::ContradictoryOrStale => "contradictory or stale",
            Self::ProductGap => "genuine product or requirement gap",
        }
    }
}

/// The seam that would fill an omission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FillSeam {
    /// A language or framework adapter (new or improved extraction).
    Adapter,
    /// A framework/SDK registry entry (known APIs, event kinds).
    Registry,
    /// A manifest input: another repository, a config file, or a runtime trace.
    ManifestInput,
    /// Interpretation: a reviewer, a semantic or agentic rung, or a decision.
    Interpretation,
    /// A requirement or product change.
    Requirement,
}

impl FillSeam {
    /// Reader-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Adapter => "adapter",
            Self::Registry => "registry",
            Self::ManifestInput => "manifest input",
            Self::Interpretation => "interpretation",
            Self::Requirement => "requirement",
        }
    }
}

/// One classified omission.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Omission {
    /// Why it is missing.
    pub class: OmissionClass,
    /// What would fill it.
    pub seam: FillSeam,
    /// The specific stop reason, verbatim.
    pub detail: String,
}

/// Ordered classification rules: the first rule with a matching fragment
/// wins. Fragments match the lowercased reason.
const RULES: &[(OmissionClass, FillSeam, &[&str])] = &[
    (
        OmissionClass::ContradictoryOrStale,
        FillSeam::Interpretation,
        &[
            "contradict",
            "conflict",
            "disagree",
            "stale",
            "distinct identities",
            "ambiguous",
        ],
    ),
    (
        OmissionClass::UnsupportedExtractor,
        FillSeam::Adapter,
        &[
            "unsupported",
            "not parsed",
            "awaits an exact mapping",
            "definition limit",
        ],
    ),
    (
        OmissionClass::MissingInput,
        FillSeam::ManifestInput,
        &[
            "runtime-computed",
            "dynamic ",
            "not found",
            "unobserved",
            "observation",
            "missing",
            "unreadable",
            "not valid json",
            "not provided by",
            "no package directory",
            "no go.mod",
            "not captured",
            "unavailable",
        ],
    ),
    (
        OmissionClass::UnresolvedRelationship,
        FillSeam::Adapter,
        &[
            "unresolved",
            "not statically resolvable",
            "no unique",
            "not provable",
            "could not be proven",
            "cannot be proven",
            "unproven",
        ],
    ),
];

impl Omission {
    /// Classify a stop reason. A reason no rule recognizes is an unresolved
    /// relationship for interpretation, and its detail says so.
    pub fn classify(reason: &str) -> Self {
        let lowered = reason.to_lowercase();
        for (class, seam, fragments) in RULES {
            if fragments.iter().any(|fragment| lowered.contains(fragment)) {
                return Self {
                    class: *class,
                    seam: *seam,
                    detail: reason.to_string(),
                };
            }
        }
        Self {
            class: OmissionClass::UnresolvedRelationship,
            seam: FillSeam::Interpretation,
            detail: if reason.trim().is_empty() {
                "unclassified: no stop reason recorded".into()
            } else {
                format!("unclassified: {reason}")
            },
        }
    }

    /// The omission a Gap fact's properties establish: an explicit, valid
    /// `omission` wins; otherwise the latest recorded stop reason
    /// (`t1_reason`, then `reason`, then `fallback`) is classified.
    pub fn of_gap(props: &serde_json::Value, fallback: &str) -> Self {
        if let Ok(explicit) = serde_json::from_value::<Self>(props["omission"].clone()) {
            return explicit;
        }
        let reason = props["t1_reason"]
            .as_str()
            .or_else(|| props["reason"].as_str())
            .unwrap_or(fallback);
        Self::classify(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_recorded_stop_reason_names_a_class_and_seam() {
        // AC-0254: real reasons from producers map to actionable classes.
        let cases = [
            (
                "env key API_URL not found in any env file in the repo",
                OmissionClass::MissingInput,
                FillSeam::ManifestInput,
            ),
            (
                "runtime-computed channel identity",
                OmissionClass::MissingInput,
                FillSeam::ManifestInput,
            ),
            (
                "package foo not provided by this repository",
                OmissionClass::MissingInput,
                FillSeam::ManifestInput,
            ),
            (
                "manifest.json is not valid JSON",
                OmissionClass::MissingInput,
                FillSeam::ManifestInput,
            ),
            (
                "unresolved Java import target",
                OmissionClass::UnresolvedRelationship,
                FillSeam::Adapter,
            ),
            (
                "callee not statically resolvable",
                OmissionClass::UnresolvedRelationship,
                FillSeam::Adapter,
            ),
            (
                "non-source file in this repository (not parsed at T0)",
                OmissionClass::UnsupportedExtractor,
                FillSeam::Adapter,
            ),
            (
                "batch correlation unsupported: 2 entries in one PutEvents call",
                OmissionClass::UnsupportedExtractor,
                FillSeam::Adapter,
            ),
            (
                "contradiction: detail type Placed is incompatible",
                OmissionClass::ContradictoryOrStale,
                FillSeam::Interpretation,
            ),
            (
                "source attribution ambiguous: an observed code path matches 2 EventBridge sites",
                OmissionClass::ContradictoryOrStale,
                FillSeam::Interpretation,
            ),
            (
                "references disagree on the boundary",
                OmissionClass::ContradictoryOrStale,
                FillSeam::Interpretation,
            ),
        ];
        for (reason, class, seam) in cases {
            let omission = Omission::classify(reason);
            assert_eq!((omission.class, omission.seam), (class, seam), "{reason}");
            assert_eq!(omission.detail, reason);
        }
    }

    #[test]
    fn unrecognized_reason_is_reported_unclassified_not_guessed() {
        let omission = Omission::classify("something new");
        assert_eq!(omission.class, OmissionClass::UnresolvedRelationship);
        assert_eq!(omission.seam, FillSeam::Interpretation);
        assert_eq!(omission.detail, "unclassified: something new");
        assert_eq!(
            Omission::classify("").detail,
            "unclassified: no stop reason recorded"
        );
    }

    #[test]
    fn explicit_omission_wins_then_latest_reason() {
        let explicit = serde_json::json!({
            "reason": "unresolved call target",
            "omission": {"class": "product_gap", "seam": "requirement", "detail": "no refund rule specified"},
        });
        assert_eq!(
            Omission::of_gap(&explicit, ""),
            Omission {
                class: OmissionClass::ProductGap,
                seam: FillSeam::Requirement,
                detail: "no refund rule specified".into()
            }
        );
        let escalated = serde_json::json!({
            "reason": "runtime-computed channel identity",
            "t1_reason": "2 distinct identities",
        });
        assert_eq!(
            Omission::of_gap(&escalated, "").class,
            OmissionClass::ContradictoryOrStale
        );
        let malformed =
            serde_json::json!({"reason": "unresolved call target", "omission": {"class": "bogus"}});
        assert_eq!(
            Omission::of_gap(&malformed, "").class,
            OmissionClass::UnresolvedRelationship
        );
        assert_eq!(
            Omission::of_gap(&serde_json::json!({}), "unresolved CALLS edge").detail,
            "unresolved CALLS edge"
        );
    }

    #[test]
    fn wire_form_is_snake_case() {
        let omission = Omission::classify("unresolved call target");
        assert_eq!(
            serde_json::to_value(&omission).unwrap(),
            serde_json::json!({"class": "unresolved_relationship", "seam": "adapter", "detail": "unresolved call target"})
        );
    }
}
