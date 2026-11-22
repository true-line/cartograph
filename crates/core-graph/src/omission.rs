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
    // ADR-0075: the value is there but no allowlist lets it be read.
    (
        "env key * is set in * but its value is not read: list it in env_read in \
         cartograph.system.toml or in .cartograph/env.toml",
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
    // Interface dispatch (`IFace.method`): the relationship is unproven
    // without a type hierarchy, and a type-resolving adapter would fill it.
    (
        "dynamic dispatch - needs type hierarchy",
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

/// Detail of an unclassified omission whose reason is blank.
const NO_STOP_REASON: &str = "no stop reason recorded";

/// Largest detail [`Omission::classify`] retains for a `reason_bytes` reason:
/// the reason itself, or [`UNCLASSIFIED_PREFIX`] before it or before the
/// blank-reason text. Logical bytes, not allocator capacity.
pub fn classified_detail_bytes(reason_bytes: usize) -> Option<usize> {
    reason_bytes
        .max(NO_STOP_REASON.len())
        .checked_add(UNCLASSIFIED_PREFIX.len())
}

/// Peak temporary owners of one [`Omission::classify`] call over a
/// `reason_bytes` reason, excluding the retained detail. Template matching
/// holds one pattern's parts. `prose_words` holds, in one expression, its
/// joined plain words (at most the reason: every separator replaces at least
/// one whitespace byte) and their lowercase. No char lowercases to more than
/// one and a half times its UTF-8 length, which a test checks for every char.
pub fn classification_workspace_bytes(reason_bytes: usize) -> Option<usize> {
    let parts = TEMPLATES
        .iter()
        .map(|(template, _, _)| template.split('*').count())
        .max()?
        .checked_mul(std::mem::size_of::<&str>())?;
    let lowercase = reason_bytes.checked_mul(3)?.div_ceil(2);
    let prose = reason_bytes.checked_add(lowercase)?;
    Some(parts.max(prose))
}

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
    // One owned buffer: a token is appended as it is read and cut off again
    // when it turns out to be identifier-like or shouty. There is no unquoted
    // copy and no token list, so the peak is this buffer plus its lowercase.
    struct Token {
        separator: usize,
        text: usize,
        identifier_like: bool,
        no_lowercase: bool,
    }
    fn close(words: &mut String, token: Option<Token>) {
        let Some(token) = token else { return };
        let shouty = words.len() - token.text > 1 && token.no_lowercase;
        if token.identifier_like || shouty {
            words.truncate(token.separator);
        }
    }
    let mut quoted = false;
    let mut words = String::new();
    let mut token: Option<Token> = None;
    for ch in reason.chars() {
        if matches!(ch, '`' | '"' | '\'') {
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }
        if ch.is_whitespace() {
            close(&mut words, token.take());
            continue;
        }
        let current = token.get_or_insert_with(|| {
            let separator = words.len();
            if separator > 0 {
                words.push(' ');
            }
            Token {
                separator,
                text: words.len(),
                identifier_like: false,
                no_lowercase: true,
            }
        });
        current.identifier_like |= ch.is_ascii_digit()
            || matches!(ch, '_' | '/' | '.' | ':' | '@' | '#' | '$' | '{' | '}');
        current.no_lowercase &= !ch.is_ascii_lowercase();
        words.push(ch);
    }
    close(&mut words, token);
    words.to_lowercase()
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
                format!("{UNCLASSIFIED_PREFIX}{NO_STOP_REASON}")
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

    /// The omission a Gap fact's properties establish (see
    /// [`Omission::recorded`]), else `fallback` classified.
    pub fn of_gap(props: &serde_json::Value, fallback: &str) -> Self {
        Self::recorded(props).unwrap_or_else(|| Self::classify(fallback))
    }

    /// The omission a Gap fact's own properties establish, if any: an
    /// explicit, valid `omission`, then `t1_reason`, then an interface
    /// dispatch hop query (AC-0285: grouped under [`DYNAMIC_DISPATCH`]
    /// rather than its older generic stop reason), then `reason`.
    pub fn recorded(props: &serde_json::Value) -> Option<Self> {
        if let Ok(explicit) = serde_json::from_value::<Self>(props["omission"].clone()) {
            return Some(explicit);
        }
        if let Some(reason) = props["t1_reason"].as_str() {
            return Some(Self::classify(reason));
        }
        if is_interface_dispatch(&gap_query(props)) {
            return Some(Self::classify(DYNAMIC_DISPATCH));
        }
        props["reason"].as_str().map(Self::classify)
    }
}

/// Conservative interface-dispatch detector for unresolved hop queries
/// (#259, AC-0285).
///
/// A query is interface dispatch when it has the shape `IFace.member`: the
/// leading segment is a receiver named in the interface convention — a
/// capital `I` followed by another capital letter and at least one further
/// character (`IChannel`, `INodeProcess`) — and a non-empty member access
/// follows it. The receiver's concrete type is unknown without a type
/// hierarchy, so no embedding can resolve the hop.
///
/// The rule is deliberately conservative: a wrong exclusion silently drops
/// a resolvable hop, while a wrong inclusion only spends an embedding, so
/// when in doubt a hop stays in candidacy. In particular:
///
/// - `Item.get`, `Icon.render` — `I` followed by a lowercase letter is a
///   plain capitalized name, not the interface convention.
/// - `IO.read`, `ID.parse` — a two-letter receiver is an acronym, not the
///   `IFoo` convention, which needs at least three characters.
/// - `IFoo` with no member access — a bare type reference is not dispatch.
/// - `channel.listen` — a lower-case receiver is a value, not a type.
pub fn is_interface_dispatch(query: &str) -> bool {
    let mut segments = query.split('.');
    let receiver = segments.next().unwrap_or_default();
    // A member access must follow the receiver.
    let Some(member) = segments.next() else {
        return false;
    };
    if member.is_empty() {
        return false;
    }
    let mut chars = receiver.chars();
    if chars.next() != Some('I') {
        return false;
    }
    // A capital `I` followed by another capital letter opens the convention.
    if !chars.next().is_some_and(|c| c.is_ascii_uppercase()) {
        return false;
    }
    // At least one further character: `IO` and `ID` are acronyms, not `IFoo`.
    chars.next().is_some()
}

/// Stop reason for a hop held out of candidacy as interface dispatch.
pub const DYNAMIC_DISPATCH: &str = "dynamic dispatch - needs type hierarchy";

/// Stop reason for a hop held out of candidacy because two or more
/// candidates carry its exact name (AC-1513): an embedding of the name
/// cannot tell homonyms apart.
pub const NAME_COLLISION: &str = "name collision - needs import or type resolution";

/// The hop query a Gap's properties establish: `raw`, then `callee`, then
/// `reason`, trimmed.
pub fn gap_query(props: &serde_json::Value) -> String {
    props["raw"]
        .as_str()
        .or_else(|| props["callee"].as_str())
        .or_else(|| props["reason"].as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The historical three-copy form, kept as the reference behavior.
    fn reference_prose_words(reason: &str) -> String {
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

    fn adversarial_reasons() -> Vec<String> {
        let mut reasons = vec![
            "a ".repeat(500),
            "a".repeat(999),
            "\u{130} ".repeat(300),
            "\u{130}".repeat(400),
            "\u{23a}\u{23e} x ".repeat(100),
            "`quoted` \"x\" 'y' a".repeat(50),
            String::new(),
            " ".into(),
            "  lead  and\ttrail\u{a0}\u{2003}\n ".into(),
            "A I be SHOUTY É ÉÉ x1 a_b src/a.ts v1.2 a:b @x #y $z {q} ok".into(),
            "un`quo ted`quoted mid\"dle\"word 'open never closes a b".into(),
            "`a` `B` '' \"\" x`y z`w".into(),
            "ΟΔΟΣ ΟΔΟΣ. οδοΣ Σ aΣ Σa aΣ'b' aΣ `x` Σ".into(),
            "stale\u{301} CAFÉ café \u{1e9e} ǅ ǆ".into(),
        ];
        // Every pair of a small alphabet covers quote, space, case and filter
        // transitions at token boundaries.
        let alphabet = ['a', 'B', ' ', '`', '_', 'Σ', '\u{130}', '\n'];
        for a in alphabet {
            for b in alphabet {
                for c in alphabet {
                    reasons.push([a, b, c, 'x', ' ', a, b].iter().collect());
                }
            }
        }
        reasons
    }

    #[test]
    fn prose_words_keep_the_reference_words_in_one_buffer() {
        // AC-0731: the single-buffer form selects exactly the historical words.
        for reason in adversarial_reasons() {
            assert_eq!(prose_words(&reason), reference_prose_words(&reason));
        }
    }

    #[test]
    fn no_char_lowercases_past_one_and_a_half_times_its_length() {
        // AC-0731: the lowercase factor, checked for every Unicode scalar
        // value. U+0130 and the two-to-three-byte mappings reach it exactly.
        let mut reached = false;
        for ch in (0..=u32::from(char::MAX)).filter_map(char::from_u32) {
            let lower: usize = ch.to_lowercase().map(char::len_utf8).sum();
            assert!(lower * 2 <= ch.len_utf8() * 3, "{ch:?}");
            reached |= lower * 2 == ch.len_utf8() * 3;
        }
        assert!(reached);
    }

    #[test]
    fn classification_workspace_covers_joined_words_and_lowercase_expansion() {
        // AC-0522, AC-0731: the joined words never exceed the reason, and
        // their lowercase never exceeds one and a half times that.
        for reason in adversarial_reasons() {
            let r = reason.len();
            let words = prose_words(&reason);
            // The joined buffer is at most the reason, so the peak is at most
            // the reason plus the lowercase of whichever is larger.
            let peak = r + words.len().max(reason.to_lowercase().len());
            assert!(reason.to_lowercase().len() * 2 <= r * 3);
            assert!(peak <= classification_workspace_bytes(r).unwrap());
            let detail = Omission::classify(&reason).detail.len();
            assert!(detail <= classified_detail_bytes(r).unwrap());
        }
        // A kept token needs one ASCII lowercase letter, so the closest kept
        // reason is that letter followed by U+0130 only: one byte under the
        // bound, and over the bound of a reason one byte shorter.
        let closest = format!("a{}", "\u{130}".repeat(400));
        let peak = closest.len() + prose_words(&closest).len();
        assert_eq!(
            peak + 1,
            classification_workspace_bytes(closest.len()).unwrap()
        );
        assert!(peak > classification_workspace_bytes(closest.len() - 1).unwrap());
        assert!(classification_workspace_bytes(usize::MAX).is_none());
    }

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
                "env key API_URL (base URL of api.get) is set in .env but its value is not \
                 read: list it in env_read in cartograph.system.toml or in .cartograph/env.toml",
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
                "dynamic dispatch - needs type hierarchy",
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
    fn interface_dispatch_gap_groups_under_its_exclusion() {
        // AC-0285 (#711 review): the register groups an interface dispatch
        // Gap under the exclusion, not its older generic stop reason.
        let dispatch =
            serde_json::json!({"callee": "IChannel.listen", "reason": "unresolved call target"});
        let omission = Omission::of_gap(&dispatch, "");
        assert_eq!(omission.detail, DYNAMIC_DISPATCH);
        assert_eq!(
            (omission.class, omission.seam),
            (OmissionClass::UnresolvedRelationship, FillSeam::Adapter)
        );
        // An explicit omission and a later tier's reason still win.
        let mut explicit = dispatch.clone();
        explicit["omission"] =
            serde_json::json!({"class": "product_gap", "seam": "requirement", "detail": "kept"});
        assert_eq!(Omission::of_gap(&explicit, "").detail, "kept");
        let mut escalated = dispatch.clone();
        escalated["t1_reason"] = "detail type unobserved".into();
        assert_eq!(
            Omission::of_gap(&escalated, "").detail,
            "detail type unobserved"
        );
        // A look-alike keeps its stop reason.
        let lookalike =
            serde_json::json!({"callee": "Item.get", "reason": "unresolved call target"});
        assert_eq!(
            Omission::of_gap(&lookalike, "").detail,
            "unresolved call target"
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
