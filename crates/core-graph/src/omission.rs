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

use FillSeam as S;
use OmissionClass as C;

/// Known producer stop-reason templates, matched against the whole reason
/// (`*` matches any non-empty run). These come first, so identifiers and
/// paths inside a known message never decide its class.
const TEMPLATES: &[(&str, OmissionClass, FillSeam)] = &[
    // Contradictory, ambiguous or rejected observations.
    (
        "multiple observed channel identities matched this source gap",
        C::ContradictoryOrStale,
        S::Interpretation,
    ),
    (
        "* distinct identities",
        C::ContradictoryOrStale,
        S::Interpretation,
    ),
    (
        "contradiction: *",
        C::ContradictoryOrStale,
        S::Interpretation,
    ),
    (
        "source attribution ambiguous*",
        C::ContradictoryOrStale,
        S::Interpretation,
    ),
    (
        "ambiguous endpoint match for *",
        C::ContradictoryOrStale,
        S::Interpretation,
    ),
    (
        "references disagree on the boundary",
        C::ContradictoryOrStale,
        S::Interpretation,
    ),
    (
        "inference rejected by Workbench curation",
        C::ContradictoryOrStale,
        S::Interpretation,
    ),
    // Missing inputs: config, traces, files, other repositories.
    (
        "env key * not found in any env file in the repo",
        C::MissingInput,
        S::ManifestInput,
    ),
    ("runtime-computed *", C::MissingInput, S::ManifestInput),
    ("dynamic * path", C::MissingInput, S::ManifestInput),
    (
        "no trace observation uniquely matched this source gap",
        C::MissingInput,
        S::ManifestInput,
    ),
    (
        "no EventBridge observation in trace",
        C::MissingInput,
        S::ManifestInput,
    ),
    (
        "source attribution missing",
        C::MissingInput,
        S::ManifestInput,
    ),
    ("detail type unobserved", C::MissingInput, S::ManifestInput),
    ("bus unobserved", C::MissingInput, S::ManifestInput),
    (
        "manifest entry not found in source tree",
        C::MissingInput,
        S::ManifestInput,
    ),
    (
        "manifest.json is not valid JSON",
        C::MissingInput,
        S::ManifestInput,
    ),
    (
        "package * not provided by this repository",
        C::MissingInput,
        S::ManifestInput,
    ),
    (
        "module not provided by any source in this repository",
        C::MissingInput,
        S::ManifestInput,
    ),
    (
        "import outside module mode (no go.mod) cannot be proven external",
        C::MissingInput,
        S::ManifestInput,
    ),
    (
        "import not provable external: *",
        C::MissingInput,
        S::ManifestInput,
    ),
    (
        "An expression was withheld or incompletely captured.",
        C::MissingInput,
        S::Adapter,
    ),
    // Constructs or limits the extractors do not support.
    (
        "batch correlation unsupported*",
        C::UnsupportedExtractor,
        S::Adapter,
    ),
    (
        "non-source file in this repository (not parsed at T0)",
        C::UnsupportedExtractor,
        S::Adapter,
    ),
    (
        "unsupported source-rule schema version: *",
        C::UnsupportedExtractor,
        S::Adapter,
    ),
    (
        "source-rule limit exceeded: *",
        C::UnsupportedExtractor,
        S::Adapter,
    ),
    ("* limit was reached*", C::UnsupportedExtractor, S::Adapter),
    (
        "Incomplete source syntax prevented supported rule analysis*",
        C::UnsupportedExtractor,
        S::Adapter,
    ),
    (
        "The initializer contains source forms outside supported structural capture.",
        C::UnsupportedExtractor,
        S::Adapter,
    ),
    (
        "Initializer recovery crosses an unsupported *",
        C::UnsupportedExtractor,
        S::Adapter,
    ),
    (
        "This binding is not a stable, directly initialized simple local const.",
        C::UnsupportedExtractor,
        S::Adapter,
    ),
    (
        "* awaits an exact mapping to original source spans.",
        C::UnsupportedExtractor,
        S::Adapter,
    ),
    // Relationships not proven by extraction.
    (
        "no recovered endpoint matches *",
        C::UnresolvedRelationship,
        S::ManifestInput,
    ),
    ("unresolved *", C::UnresolvedRelationship, S::Adapter),
    (
        "* unresolved call target",
        C::UnresolvedRelationship,
        S::Adapter,
    ),
    (
        "callee not statically resolvable",
        C::UnresolvedRelationship,
        S::Adapter,
    ),
    (
        "no unique overload proves this call's argument count",
        C::UnresolvedRelationship,
        S::Adapter,
    ),
    (
        "import of a repository * with no *",
        C::UnresolvedRelationship,
        S::Adapter,
    ),
    (
        "const-shaped code argument could not be proven to a literal",
        C::UnresolvedRelationship,
        S::Adapter,
    ),
    (
        "A call prerequisite lacks a proven target*",
        C::UnresolvedRelationship,
        S::Adapter,
    ),
    (
        "A value prerequisite lacks a stable scope-proven declaration.",
        C::UnresolvedRelationship,
        S::Adapter,
    ),
    // Behavior extraction found but did not interpret.
    (
        "weak inference excluded by verified-only export",
        C::UnresolvedRelationship,
        S::Interpretation,
    ),
    (
        "* has not been established.",
        C::UnresolvedRelationship,
        S::Interpretation,
    ),
    (
        "* was not established.",
        C::UnresolvedRelationship,
        S::Interpretation,
    ),
    (
        "* has not been interpreted.",
        C::UnresolvedRelationship,
        S::Interpretation,
    ),
    (
        "* has not been analyzed.",
        C::UnresolvedRelationship,
        S::Interpretation,
    ),
    (
        "* is unknown.",
        C::UnresolvedRelationship,
        S::Interpretation,
    ),
    (
        "Source-rule interpretation remains incomplete.",
        C::UnresolvedRelationship,
        S::Interpretation,
    ),
];

/// Keyword rules for reasons no template knows, applied to the reason with
/// identifier-, path- and quote-like tokens removed (see [`prose_words`]).
const KEYWORDS: &[(OmissionClass, FillSeam, &[&str])] = &[
    (
        C::ContradictoryOrStale,
        S::Interpretation,
        &["contradict", "conflict", "disagree", "stale", "ambiguous"],
    ),
    (
        C::UnsupportedExtractor,
        S::Adapter,
        &["unsupported", "not parsed", "limit"],
    ),
    (
        C::MissingInput,
        S::ManifestInput,
        &[
            "not found",
            "unobserved",
            "missing",
            "unreadable",
            "not provided",
            "unavailable",
        ],
    ),
    (
        C::UnresolvedRelationship,
        S::Adapter,
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

/// Prefix marking an omission no rule could classify.
pub const UNCLASSIFIED_PREFIX: &str = "unclassified: ";

/// Whole-string glob match where each `*` matches a non-empty run.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || text.len() < first.len() + last.len() || !text.ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for middle in &parts[1..parts.len() - 1] {
        // Each wildcard before `middle` consumes at least one byte.
        let Some(found) = rest.get(1..).and_then(|tail| tail.find(middle)) else {
            return false;
        };
        rest = &rest[1 + found + middle.len()..];
    }
    !rest.is_empty()
}

/// The reason's plain words: tokens that look like identifiers, paths,
/// URLs, numbers or quoted values are dropped, so a variable such as
/// `STALE_API_URL` or `src/conflict.ts` cannot select a class.
fn prose_words(reason: &str) -> String {
    let mut quoted = false;
    let unquoted: String = reason
        .chars()
        .filter(|ch| {
            if matches!(ch, '`' | '"' | '\'') {
                quoted = !quoted;
                return false;
            }
            !quoted
        })
        .collect();
    unquoted
        .split_whitespace()
        .filter(|token| {
            let identifier_like = token.chars().any(|ch| {
                ch.is_ascii_digit()
                    || matches!(ch, '_' | '/' | '.' | ':' | '@' | '#' | '$' | '{' | '}')
            });
            let shouty = token.len() > 1 && token.chars().all(|ch| !ch.is_ascii_lowercase());
            !identifier_like && !shouty
        })
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl Omission {
    /// Classify a stop reason: a known producer template first, then
    /// keywords over its plain words. A reason neither recognizes is an
    /// unresolved relationship for interpretation whose detail starts with
    /// [`UNCLASSIFIED_PREFIX`], so it is never shown as established.
    pub fn classify(reason: &str) -> Self {
        let found = |class, seam| Self {
            class,
            seam,
            detail: reason.to_string(),
        };
        if let Some((_, class, seam)) = TEMPLATES
            .iter()
            .find(|(template, _, _)| glob_matches(template, reason))
        {
            return found(*class, *seam);
        }
        let words = prose_words(reason);
        for (class, seam, fragments) in KEYWORDS {
            if fragments.iter().any(|fragment| words.contains(fragment)) {
                return found(*class, *seam);
            }
        }
        Self {
            class: OmissionClass::UnresolvedRelationship,
            seam: FillSeam::Interpretation,
            detail: if reason.trim().is_empty() {
                format!("{UNCLASSIFIED_PREFIX}no stop reason recorded")
            } else {
                format!("{UNCLASSIFIED_PREFIX}{reason}")
            },
        }
    }

    /// Whether no rule could classify this omission.
    pub fn is_unclassified(&self) -> bool {
        self.detail.starts_with(UNCLASSIFIED_PREFIX)
    }

    /// The class label, marked when the class was not established.
    pub fn class_label(&self) -> String {
        if self.is_unclassified() {
            format!("unclassified ({})", self.class.label())
        } else {
            self.class.label().to_string()
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
    fn identifiers_and_paths_never_decide_the_class() {
        // #603 review: the variable part of a producer message is data.
        for key in [
            "API_URL",
            "STALE_API_URL",
            "CONFLICT_SERVICE_URL",
            "UNSUPPORTED_API_URL",
        ] {
            let reason = format!("env key {key} not found in any env file in the repo");
            let omission = Omission::classify(&reason);
            assert_eq!(
                (omission.class, omission.seam),
                (OmissionClass::MissingInput, FillSeam::ManifestInput),
                "{reason}"
            );
        }
        // Unknown messages fall back to keywords over plain words only.
        assert_eq!(
            Omission::classify("handler in src/stale/conflict.ts not wired").class,
            OmissionClass::UnresolvedRelationship
        );
        assert!(Omission::classify("handler in src/stale/conflict.ts not wired").is_unclassified());
        assert_eq!(
            Omission::classify("value `stale` could not be proven").class,
            OmissionClass::UnresolvedRelationship
        );
        assert!(!Omission::classify("value `stale` could not be proven").is_unclassified());
    }

    #[test]
    fn glob_wildcards_match_non_empty_runs() {
        assert!(glob_matches("unresolved *", "unresolved call target"));
        assert!(!glob_matches("unresolved *", "unresolved "));
        assert!(glob_matches(
            "* distinct identities",
            "2 distinct identities"
        ));
        assert!(glob_matches(
            "import of a repository * with no *",
            "import of a repository module path with no package directory"
        ));
        assert!(!glob_matches(
            "import of a repository * with no *",
            "import of a repository with no package"
        ));
        assert!(glob_matches("exact", "exact") && !glob_matches("exact", "exactly"));
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
