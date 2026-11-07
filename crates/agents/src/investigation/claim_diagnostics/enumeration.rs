//! Conservative checks over literal enumerations, not a natural-language truth oracle.
use crate::investigation::enumeration_claims::claims::literal;
use literal::Enumeration;
#[path = "shape.rs"]
mod shape;

use crate::investigation::claim_diagnostics::{Cause, Explanation, Position, Qualifier, Rule};

/// Preserve historical applicability, traversal and verdicts. The richer result
/// is internal host evidence; no change to model-facing correction feedback.
#[cfg(test)]
pub(super) fn check(source: &str, sentence: &str) -> Option<bool> {
    explain(source, sentence).map(|result| result.is_ok())
}
pub(crate) fn explain(source: &str, sentence: &str) -> Option<Result<(), Explanation>> {
    let enumeration = literal::parse(source)?;
    let mut applicable = false;
    let mut token_offset = 0;
    for clause in sentence.split(", and ") {
        if let Some(result) = check_clause(&enumeration, clause) {
            match result {
                Err(mut reason) => {
                    reason.quantifier_token = tokens(clause)
                        .iter()
                        .position(|word| {
                            crate::investigation::claim_diagnostics::GrammarWord::from_word(word)
                                .is_some_and(|word| word.is_quantifier())
                        })
                        .map(|index| index + token_offset);
                    reason.target_token = reason.target_token.map(|i| i + token_offset);
                    return Some(Err(reason));
                }
                Ok(()) => applicable = true,
            }
        }
        // The separator's `and` is one token in the full sentence tokenizer.
        token_offset += tokens(clause).len() + 1;
    }
    applicable.then_some(Ok(()))
}
pub(super) fn describe(
    source: &str,
    sentence: &str,
    allocation_cap: usize,
) -> Option<(
    crate::investigation::claim_diagnostics::TableKind,
    crate::investigation::claim_diagnostics::Shape,
)> {
    use crate::investigation::claim_diagnostics::{ShapeToken, TableKind};
    let table = literal::parse(source)?;
    let kind = match &table {
        Enumeration::Array(_) => TableKind::Array,
        Enumeration::Table(_) => TableKind::Table,
    };
    Some((
        kind,
        shape::project(
            &table,
            sentence,
            allocation_cap / std::mem::size_of::<ShapeToken>(),
            allocation_cap / std::mem::size_of::<Position>(),
        ),
    ))
}

pub(super) fn quantifier_at(
    sentence: &str,
    index: usize,
) -> Option<crate::investigation::claim_diagnostics::GrammarWord> {
    crate::investigation::claim_diagnostics::GrammarWord::from_word(tokens(sentence).get(index)?)
}
fn tokens(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .filter(|word| !word.is_empty())
        .collect()
}
fn refusal(
    cause: Cause,
    rule: Rule,
    counterexample: Vec<Position>,
    target_token: Option<usize>,
) -> Result<(), Explanation> {
    Err(Explanation {
        cause,
        rule,
        counterexample,
        target_token,
        quantifier_token: None,
    })
}
fn outside(rule: Rule) -> Result<(), Explanation> {
    refusal(Cause::OutsideGrammar, rule, vec![], None)
}
fn check_clause(table: &Enumeration<'_>, text: &str) -> Option<Result<(), Explanation>> {
    let words = tokens(text);
    let lower: Vec<_> = words.iter().map(|word| word.to_ascii_lowercase()).collect();
    let quantifier = lower.iter().position(|word| {
        crate::investigation::claim_diagnostics::GrammarWord::from_word(word)
            .is_some_and(|word| word.is_quantifier())
    })?;
    let identifiers: Vec<_> = match table {
        Enumeration::Array(values) => values.clone(),
        Enumeration::Table(entries) => entries
            .iter()
            .flat_map(|(key, values)| std::iter::once(*key).chain(values.iter().copied()))
            .collect(),
    };
    if !words.iter().any(|word| identifiers.contains(word)) {
        return None;
    }
    let rule = match table {
        Enumeration::Array(_) => match lower.first().map(String::as_str) {
            Some("every" | "all" | "each") => Rule::ArrayAll,
            Some("only") => Rule::ArrayOnly,
            _ => Rule::Unsupported,
        },
        Enumeration::Table(_) => match (lower[quantifier].as_str(), quantifier) {
            ("every" | "all" | "each", 0) => Rule::TableAll,
            ("only", 0) => Rule::TableOnly,
            ("no", 2) => Rule::TableEmpty,
            _ => Rule::Unsupported,
        },
    };
    for word in &lower {
        let qualifier = match word.as_str() {
            "except" => Qualifier::Except,
            "unless" => Qualifier::Unless,
            "not" => Qualifier::Not,
            "without" => Qualifier::Without,
            "if" => Qualifier::If,
            _ => continue,
        };
        return Some(refusal(
            Cause::QualifierPresent(qualifier),
            rule,
            vec![],
            None,
        ));
    }
    if let Enumeration::Array(values) = table {
        let target = match lower.as_slice() {
            [q, domain, verb, _]
                if matches!(q.as_str(), "every" | "all" | "each")
                    && matches!(domain.as_str(), "entry" | "entries" | "value" | "values")
                    && matches!(verb.as_str(), "is" | "are") =>
            {
                Some(3)
            }
            [only, _, rest @ ..] if only == "only" && rest == ["is", "listed"] => Some(1),
            _ => None,
        };
        let Some(target) = target else {
            return Some(outside(rule));
        };
        if let Some(value) = values.iter().position(|value| *value != words[target]) {
            return Some(refusal(
                Cause::RelationFalse,
                rule,
                vec![Position::Value { key: None, value }],
                Some(target),
            ));
        }
        // A bound only-array claim cannot reach an empty table because its
        // identifier applicability guard above would return None.
        return Some(Ok(()));
    }
    let Enumeration::Table(entries) = table else {
        unreachable!()
    };
    let result = match rule {
        Rule::TableAll => {
            let predicate = lower.iter().position(|word| {
                matches!(
                    word.as_str(),
                    "lists" | "list" | "allows" | "allow" | "permits" | "permit"
                )
            });
            let Some(predicate) = predicate.filter(|p| *p > 1) else {
                return Some(outside(rule));
            };
            let domain = lower[1..predicate].join(" ");
            if !matches!(
                domain.as_str(),
                "state"
                    | "states"
                    | "entry"
                    | "entries"
                    | "key"
                    | "keys"
                    | "state entry"
                    | "state entries"
            ) {
                return Some(outside(rule));
            }
            let Some(targets) = target_list(&words[predicate + 1..]) else {
                return Some(outside(rule));
            };
            let failed = targets
                .iter()
                .enumerate()
                .find_map(|(target_index, target)| {
                    entries
                        .iter()
                        .position(|(_, values)| !values.contains(target))
                        .map(|key| (key, predicate + 1 + target_index * 2))
                });
            match failed {
                Some((key, target)) => refusal(
                    Cause::RelationFalse,
                    rule,
                    vec![Position::Key(key)],
                    Some(target),
                ),
                None => Ok(()),
            }
        }
        Rule::TableOnly => {
            let key_index = if lower.get(1).is_some_and(|w| w == "the") {
                2
            } else {
                1
            };
            let key = words
                .get(key_index)
                .and_then(|key| entries.iter().position(|(candidate, _)| candidate == key));
            let Some(key) = key else {
                return Some(outside(rule));
            };
            let predicate =
                key_index + 1 + usize::from(lower.get(key_index + 1).is_some_and(|w| w == "state"));
            if !lower
                .get(predicate)
                .is_some_and(|w| matches!(w.as_str(), "lists" | "allows" | "permits"))
            {
                return Some(outside(rule));
            }
            let Some(targets) = target_list(&words[predicate + 1..]) else {
                return Some(outside(rule));
            };
            if let Some(index) = targets
                .iter()
                .position(|target| !entries[key].1.contains(target))
            {
                return Some(refusal(
                    Cause::RelationFalse,
                    rule,
                    vec![Position::Key(key)],
                    Some(predicate + 1 + index * 2),
                ));
            }
            let other = entries.iter().enumerate().find_map(|(other, (_, values))| {
                if other == key {
                    return None;
                }
                targets.iter().enumerate().find_map(|(t, target)| {
                    values
                        .iter()
                        .position(|v| v == target)
                        .map(|value| (other, value, t))
                })
            });
            match other {
                Some((key, value, t)) => refusal(
                    Cause::RelationFalse,
                    rule,
                    vec![
                        Position::Key(key),
                        Position::Value {
                            key: Some(key),
                            value,
                        },
                    ],
                    Some(predicate + 1 + t * 2),
                ),
                None => Ok(()),
            }
        }
        Rule::TableEmpty => {
            let key = entries
                .iter()
                .position(|(key, _)| Some(key) == words.first());
            let Some(key) = key else {
                return Some(outside(rule));
            };
            if !matches!(lower[1].as_str(), "lists" | "permits" | "allows" | "has")
                || !matches!(
                    lower[3..].join(" ").as_str(),
                    "transitions"
                        | "further transitions"
                        | "outgoing transitions"
                        | "targets"
                        | "further targets"
                        | "outgoing targets"
                )
            {
                return Some(outside(rule));
            }
            if entries[key].1.is_empty() {
                Ok(())
            } else {
                refusal(
                    Cause::RelationFalse,
                    rule,
                    vec![
                        Position::Key(key),
                        Position::Value {
                            key: Some(key),
                            value: 0,
                        },
                    ],
                    None,
                )
            }
        }
        _ => outside(rule),
    };
    Some(result)
}

// A positive relation must account for every token. Arbitrary predicates,
// qualifiers and trailing claims cannot piggyback on identifier co-occurrence.
fn target_list<'a>(words: &[&'a str]) -> Option<Vec<&'a str>> {
    let mut targets = Vec::new();
    let mut expect_target = true;
    for word in words {
        if expect_target {
            if matches!(*word, "and" | "or") {
                return None;
            }
            targets.push(*word);
        } else if !matches!(*word, "and" | "or") {
            return None;
        }
        expect_target = !expect_target;
    }
    (!expect_target && !targets.is_empty()).then_some(targets)
}

#[cfg(test)]
mod tests {
    use super::*;
    const SOURCE: &str = include_str!("../enumeration_claims/r3-transitions.txt");

    // AC-0287 / #1707: classification adds no grammar or verdict changes.
    #[test]
    fn reason_codes_distinguish_grammar_qualifier_and_counterexample() {
        fn why(source: &str, sentence: &str) -> Explanation {
            explain(source, sentence).unwrap().unwrap_err()
        }
        assert_eq!(
            why(SOURCE, "Every pre-cancellation state lists Cancelled").cause,
            Cause::OutsideGrammar
        );
        assert_eq!(
            why(
                SOURCE,
                "Cancelled is terminal and has no outgoing transitions"
            )
            .rule,
            Rule::Unsupported
        );
        assert_eq!(
            why(SOURCE, "Every state except Created lists Cancelled").cause,
            Cause::QualifierPresent(Qualifier::Except)
        );
        let reason = why("{A: ['Target'], B: ['Target']}", "Only A lists Target");
        assert_eq!(reason.cause, Cause::RelationFalse);
        assert_eq!(reason.rule, Rule::TableOnly);
        assert_eq!(
            reason.counterexample,
            vec![
                Position::Key(1),
                Position::Value {
                    key: Some(1),
                    value: 0
                }
            ]
        );
        assert_eq!(reason.target_token, Some(3));
        let reason = why("{A: ['Target'], B: []}", "Every state lists Target");
        assert_eq!(reason.counterexample, vec![Position::Key(1)]);
        assert_eq!(reason.target_token, Some(3));
        assert_eq!(
            why("['Target', 'Other']", "All entries are Target").counterexample,
            vec![Position::Value {
                key: None,
                value: 1
            }]
        );
        for (word, qualifier) in [
            ("unless", Qualifier::Unless),
            ("not", Qualifier::Not),
            ("without", Qualifier::Without),
            ("if", Qualifier::If),
        ] {
            let sentence = format!("Every state {word} Created lists Cancelled");
            assert_eq!(
                why(SOURCE, &sentence).cause,
                Cause::QualifierPresent(qualifier)
            );
        }
    }

    // AC-0287: exact r3 text distinguishes false universals from ordinary prose.
    #[test]
    fn r3_claims_and_true_empty_list() {
        assert_eq!(
            check(
                SOURCE,
                "The transition table shows only the ArrangingPayment state can begin payment authorization or settlement, and that delivery and shipping states branch from PaymentSettled."
            ),
            Some(false)
        );
        assert_eq!(
            check(
                SOURCE,
                "Every pre-cancellation state lists Cancelled as a permitted target, indicating cancellation is a universal exit available throughout the pre-cancelled lifecycle, while Cancelled is terminal in this table."
            ),
            Some(false)
        );
        assert_eq!(
            check(
                SOURCE,
                "Each state entry lists only the target states it allows, and Cancelled permits no further transitions."
            ),
            Some(true)
        );
        assert_eq!(
            check(SOURCE, "Cancelled lists no further transitions"),
            Some(true)
        );
        assert_eq!(
            check(
                SOURCE,
                "Cancelled is terminal and has no outgoing transitions"
            ),
            Some(false)
        );
        assert_eq!(
            check(
                SOURCE,
                "Every state except Created and Modifying lists Cancelled"
            ),
            Some(false)
        );
        assert_eq!(
            check(
                SOURCE,
                "Each state entry lists only the target states it allows"
            ),
            None
        );
    }

    #[test]
    fn bare_arrays_require_explicit_membership_predicates() {
        assert_eq!(check("['Target']", "All entries are Target"), Some(true));
        assert_eq!(
            check("['Target', 'Other']", "All entries are Target"),
            Some(false)
        );
        assert_eq!(check("['Target']", "Only Target is listed"), Some(true));
        assert_eq!(
            check("['Target']", "Only Target authorizes payment"),
            Some(false)
        );
    }

    #[test]
    fn explicit_true_and_false_relations() {
        let source = "{A: ['Target'], B: ['Target']}";
        assert_eq!(check(source, "Every state lists Target"), Some(true));
        assert_eq!(check(source, "Only A lists Target"), Some(false));
        assert_eq!(
            check("{A: ['Target'], B: []}", "Only A lists Target"),
            Some(true)
        );
        assert_eq!(check(source, "A has no outgoing transitions"), Some(false));
        assert_eq!(
            check(source, "Every selected state lists Target"),
            Some(false)
        );
        assert_eq!(
            check(source, "Every state does not list Target"),
            Some(false)
        );
        assert_eq!(check(source, "Only unrelated prose is available"), None);
        for claim in [
            "Only A forbids Target but lists Target",
            "Only A lists Target conditionally",
            "Every state lists Target while preventing it",
            "B says A has no outgoing transitions",
        ] {
            assert_eq!(check(source, claim), Some(false), "{claim}");
        }
    }
}
