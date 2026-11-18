//! Versioned host-only diagnostics. No type can retain an arbitrary prose token.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use super::MAX_MANIFEST_BYTES;
use serde::{Deserialize, Serialize};
pub(super) mod configuration;
pub(super) mod enumeration;
pub use configuration::Witness as ConfigurationWitness;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnumerationDiagnostic {
    pub table_kind: TableKind,
    pub quantifier: Option<GrammarWord>,
    pub explanation: Explanation,
    pub shape: Shape,
}

/// Explain a refused literal-enumeration claim without retaining authored words.
/// This optional diagnostic API has no admission authority and is not called by
/// production admission. The allocation cap applies independently to token
/// slots and position slots; it is not a serialized-byte budget. Host callers
/// must bound the complete serialized envelope separately before retention.
pub fn explain_enumeration(
    source: &str,
    sentence: &str,
    allocation_cap: usize,
) -> Option<EnumerationDiagnostic> {
    explain_enumeration_with_grammar(source, sentence, allocation_cap, false)
}

pub(crate) fn explain_enumeration_with_grammar(
    source: &str,
    sentence: &str,
    allocation_cap: usize,
    bounded: bool,
) -> Option<EnumerationDiagnostic> {
    let result = if bounded {
        enumeration::explain_bounded(source, sentence)
    } else {
        enumeration::explain(source, sentence)
    }?;
    let Err(mut explanation) = result else {
        return None;
    };
    let (table_kind, shape) =
        enumeration::describe(source, sentence, allocation_cap.min(MAX_MANIFEST_BYTES))?;
    // Resolved from the full sentence, so it survives shape truncation.
    let quantifier = explanation
        .quantifier_token
        .and_then(|index| enumeration::quantifier_at(sentence, index));
    // Token references always index retained `shape.tokens`. A reference to an
    // omitted token is cleared; `shape.omitted_tokens` records the truncation.
    let retained = shape.tokens.len();
    for index in [
        &mut explanation.target_token,
        &mut explanation.quantifier_token,
    ] {
        if index.is_some_and(|index| index >= retained) {
            *index = None;
        }
    }
    Some(EnumerationDiagnostic {
        table_kind,
        quantifier,
        explanation,
        shape,
    })
}

/// Syntactic witness search only: failure is not proof of a missing runtime binding.
pub fn explain_configuration(
    path: &str,
    source: &str,
    identifier: &str,
) -> Result<ConfigurationWitness, ConfigurationFailure> {
    configuration::explain(path, source, identifier)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Qualifier {
    Except,
    Unless,
    Not,
    Without,
    If,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    ArrayAll,
    ArrayOnly,
    TableAll,
    TableOnly,
    TableEmpty,
    Unsupported,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    RelationFalse,
    OutsideGrammar,
    QualifierPresent(Qualifier),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Position {
    Key(usize),
    Value { key: Option<usize>, value: usize },
}
/// A missing target has no source position; its token index points into the
/// retained text-free `shape.tokens`, and is `None` when that token was omitted
/// by truncation. The counterexample key still identifies the falsifying row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Explanation {
    pub cause: Cause,
    pub rule: Rule,
    pub counterexample: Vec<Position>,
    pub target_token: Option<usize>,
    pub quantifier_token: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrammarWord {
    Every,
    All,
    Each,
    Any,
    Only,
    None,
    No,
    Universal,
    Sole,
    Except,
    Unless,
    Not,
    Without,
    If,
    Entry,
    Entries,
    Value,
    Values,
    Is,
    Are,
    Listed,
    Lists,
    List,
    Allows,
    Allow,
    Permits,
    Permit,
    State,
    States,
    Key,
    Keys,
    The,
    Has,
    Transitions,
    Further,
    Outgoing,
    Targets,
    And,
    Or,
}
impl GrammarWord {
    pub(crate) fn is_quantifier(self) -> bool {
        matches!(
            self,
            Self::Every
                | Self::All
                | Self::Each
                | Self::Any
                | Self::Only
                | Self::None
                | Self::No
                | Self::Universal
                | Self::Sole
        )
    }
    pub(crate) fn from_word(word: &str) -> Option<Self> {
        match word.to_ascii_lowercase().as_str() {
            "every" => Some(Self::Every),
            "all" => Some(Self::All),
            "each" => Some(Self::Each),
            "any" => Some(Self::Any),
            "only" => Some(Self::Only),
            "none" => Some(Self::None),
            "no" => Some(Self::No),
            "universal" => Some(Self::Universal),
            "sole" => Some(Self::Sole),
            "except" => Some(Self::Except),
            "unless" => Some(Self::Unless),
            "not" => Some(Self::Not),
            "without" => Some(Self::Without),
            "if" => Some(Self::If),
            "entry" => Some(Self::Entry),
            "entries" => Some(Self::Entries),
            "value" => Some(Self::Value),
            "values" => Some(Self::Values),
            "is" => Some(Self::Is),
            "are" => Some(Self::Are),
            "listed" => Some(Self::Listed),
            "lists" => Some(Self::Lists),
            "list" => Some(Self::List),
            "allows" => Some(Self::Allows),
            "allow" => Some(Self::Allow),
            "permits" => Some(Self::Permits),
            "permit" => Some(Self::Permit),
            "state" => Some(Self::State),
            "states" => Some(Self::States),
            "key" => Some(Self::Key),
            "keys" => Some(Self::Keys),
            "the" => Some(Self::The),
            "has" => Some(Self::Has),
            "transitions" => Some(Self::Transitions),
            "further" => Some(Self::Further),
            "outgoing" => Some(Self::Outgoing),
            "targets" => Some(Self::Targets),
            "and" => Some(Self::And),
            "or" => Some(Self::Or),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeToken {
    Vocabulary(GrammarWord),
    Identifier(Vec<Position>),
    Word,
    Number,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shape {
    pub tokens: Vec<ShapeToken>,
    pub omitted_tokens: usize,
    pub omitted_positions: usize,
    pub matched_identifiers: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableKind {
    Array,
    Table,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationFailure {
    SourceTokenizationFailed,
    SubjectUnbound,
    BindingNotRecognized,
}
#[cfg(test)]
mod tests {
    use super::*;

    // #1707 review: truncation never leaves a token reference outside the
    // retained shape; the quantifier word itself is still reported.
    #[test]
    fn truncated_shape_has_no_dangling_token_references() {
        let full = explain_enumeration("['A', 'B']", "All entries are A", 4096).unwrap();
        assert_eq!(full.explanation.target_token, Some(3));
        assert_eq!(full.explanation.quantifier_token, Some(0));
        let cut = explain_enumeration("['A', 'B']", "All entries are A", 0).unwrap();
        assert!(cut.shape.tokens.is_empty());
        assert_eq!(cut.shape.omitted_tokens, 4);
        assert_eq!(cut.explanation.target_token, None);
        assert_eq!(cut.explanation.quantifier_token, None);
        assert_eq!(cut.quantifier, Some(GrammarWord::All));
        assert_eq!(cut.explanation.cause, full.explanation.cause);
        assert_eq!(
            cut.explanation.counterexample,
            full.explanation.counterexample
        );
    }

    // AC-0287 / #1707: the wire format cannot turn arbitrary refused words into
    // literals by claiming they belong to the detector vocabulary.
    #[test]
    fn diagnostic_tokens_have_a_closed_wire_vocabulary() {
        let value = ShapeToken::Vocabulary(GrammarWord::Only);
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            r#"{"vocabulary":"only"}"#
        );
        for wire in [
            r#"{"vocabulary":"private_customer_name"}"#,
            r#"{"vocabulary":"私密"}"#,
            r#"{"word":"refused prose"}"#,
            r#"{"identifier":[{"key":"Secret"}]}"#,
        ] {
            assert!(serde_json::from_str::<ShapeToken>(wire).is_err());
        }
        assert_eq!(
            serde_json::from_str::<ShapeToken>(r#"{"vocabulary":"only"}"#).unwrap(),
            value
        );
    }
}

#[cfg(test)]
#[path = "claim_diagnostics/r3_shapes.rs"]
mod r3_shapes;

/// Finding-local diagnostic event. Span ordinals refer only to supplied citations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimDiagnostic {
    Enumeration {
        span: usize,
        diagnostic: EnumerationDiagnostic,
    },
    Configuration {
        predicate_token: usize,
        failures: Vec<(usize, ConfigurationFailure)>,
    },
}

/// Visit the same sentence and subject grammar used by refusal detection.
/// The caller supplies authenticated finding-local spans and bounds retention.
pub fn visit_claim_diagnostics(
    title: &str,
    statement: &str,
    spans: &[(&str, &str)],
    allocation_cap: usize,
    visit: impl FnMut(bool, usize, ClaimDiagnostic),
) {
    super::enumeration_claims::visit_diagnostics(title, statement, spans, allocation_cap, visit);
}
