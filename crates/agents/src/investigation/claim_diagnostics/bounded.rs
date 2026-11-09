//! Exact additional predicates selected only by analyst @27.
use super::{Cause, Enumeration, Explanation, Position, Rule, outside, refusal, target_list};

pub(super) fn check(table: &Enumeration<'_>, words: &[&str]) -> Option<Result<(), Explanation>> {
    let Enumeration::Table(entries) = table else {
        return None;
    };
    let lower: Vec<_> = words.iter().map(|word| word.to_ascii_lowercase()).collect();
    let tail: Vec<_> = lower.iter().skip(1).map(String::as_str).collect();
    let terminal = tail.starts_with(&["is", "terminal"]);
    let exclusive = tail.starts_with(&["is", "the", "only", "state"])
        || tail.starts_with(&["is", "the", "sole", "state"]);
    let rule = if terminal {
        Rule::TableEmpty
    } else if exclusive {
        Rule::TableOnly
    } else {
        return None;
    };
    let Some(key) = entries
        .iter()
        .position(|(key, _)| Some(key) == words.first())
    else {
        return Some(outside(rule));
    };
    if terminal {
        if tail != ["is", "terminal"]
            && tail
                != [
                    "is",
                    "terminal",
                    "and",
                    "has",
                    "no",
                    "outgoing",
                    "transitions",
                ]
        {
            return Some(outside(rule));
        }
        return Some(if entries[key].1.is_empty() {
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
        });
    }
    if tail.get(4..6) != Some(&["that", "lists"][..]) {
        return Some(outside(rule));
    }
    let Some(targets) = words.get(7..).and_then(target_list) else {
        return Some(outside(rule));
    };
    for (index, target) in targets.iter().enumerate() {
        if !entries[key].1.contains(target) {
            return Some(refusal(
                Cause::RelationFalse,
                rule,
                vec![Position::Key(key)],
                Some(7 + 2 * index),
            ));
        }
    }
    for (other, (_, values)) in entries.iter().enumerate() {
        if other == key {
            continue;
        }
        for (index, target) in targets.iter().enumerate() {
            if let Some(value) = values.iter().position(|value| value == target) {
                return Some(refusal(
                    Cause::RelationFalse,
                    rule,
                    vec![
                        Position::Key(other),
                        Position::Value {
                            key: Some(other),
                            value,
                        },
                    ],
                    Some(7 + 2 * index),
                ));
            }
        }
    }
    Some(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::super::{literal, tokens};
    use super::*;

    fn verdict(source: &str, sentence: &str) -> Option<Result<(), Explanation>> {
        check(&literal::parse(source).unwrap(), &tokens(sentence))
    }

    // AC-0287 / #1729: shared traversal keeps positions in original prose,
    // without changing the production entry point before version selection.
    #[test]
    fn bounded_diagnostics_keep_original_positions_and_legacy_verdicts() {
        use super::super::explain_bounded as explain;
        let source = "{A: ['T'], B: ['T'], C: []}";
        let reason = explain(
            source,
            "C is terminal, and A is the sole state that lists T",
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(reason.target_token, Some(11));
        assert_eq!(reason.quantifier_token, Some(7));
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
        assert_eq!(reason.rule, Rule::TableOnly);
        let terminal = explain(source, "A is terminal").unwrap().unwrap_err();
        assert_eq!(terminal.quantifier_token, None);
        assert_eq!(terminal.target_token, None);
        assert_eq!(terminal.rule, Rule::TableEmpty);
        let compound = explain(source, "A is terminal and has no outgoing transitions")
            .unwrap()
            .unwrap_err();
        assert_eq!(compound.quantifier_token, Some(5));
        for claim in [
            "Every state lists T",
            "Every state except C lists T",
            "Only A lists T",
            "Only A forbids T but lists T",
        ] {
            assert_eq!(
                explain(source, claim),
                super::super::explain(source, claim),
                "{claim}"
            );
        }
        assert_eq!(super::super::explain(source, "C is terminal"), None);
        assert_eq!(explain(source, "C is terminal"), Some(Ok(())));
    }

    // AC-0287 / #1729: every sibling of a bounded clause must be checked.
    #[test]
    fn bounded_predicates_reject_unchecked_sibling_clauses() {
        use super::super::{explain, explain_bounded};
        let source = "{A: [], B: ['T']}";
        for predicate in ["A is terminal", "B is the sole state that lists T"] {
            for claim in [
                format!("{predicate}, and executes payments"),
                format!("executes payments, and {predicate}"),
                format!("{predicate}, and "),
            ] {
                let reason = explain_bounded(source, &claim).unwrap().unwrap_err();
                assert_eq!(reason.cause, Cause::OutsideGrammar, "{claim}");
                assert_eq!(reason.rule, Rule::Unsupported, "{claim}");
            }
        }
        assert_eq!(
            explain_bounded(
                source,
                "A is terminal, and B is the sole state that lists T"
            ),
            Some(Ok(()))
        );
        // The old entry point and legacy-only sentences retain applicability.
        let legacy = "Only B lists T, and executes payments";
        assert_eq!(explain(source, legacy), Some(Ok(())));
        assert_eq!(explain_bounded(source, legacy), explain(source, legacy));
        assert_eq!(
            explain(source, "A is terminal, and executes payments"),
            None
        );
    }

    // AC-0287 / #1729: exact empty-row truth, including quantifier-free form.
    #[test]
    fn bounded_terminal_contrasts() {
        for claim in [
            "A is terminal",
            "A is terminal and has no outgoing transitions",
        ] {
            assert_eq!(verdict("{A: [], B: ['T']}", claim), Some(Ok(())));
            let reason = verdict("{A: ['T'], B: []}", claim).unwrap().unwrap_err();
            assert_eq!(reason.cause, Cause::RelationFalse);
            assert_eq!(reason.rule, Rule::TableEmpty);
            assert_eq!(
                reason.counterexample,
                vec![
                    Position::Key(0),
                    Position::Value {
                        key: Some(0),
                        value: 0
                    }
                ]
            );
            assert_eq!(
                verdict("{B: []}", claim).unwrap().unwrap_err().cause,
                Cause::OutsideGrammar
            );
            assert_eq!(verdict("['A']", claim), None);
        }
    }

    // AC-0287 / #1729: only the subject is exclusive, not its outgoing set.
    #[test]
    fn bounded_exclusive_contrasts() {
        for quantifier in ["only", "sole"] {
            for conjunction in ["and", "or"] {
                let claim = format!("A is the {quantifier} state that lists T {conjunction} U");
                assert_eq!(
                    verdict("{A: ['T', 'U', 'Extra'], B: []}", &claim),
                    Some(Ok(()))
                );
                let reason = verdict("{A: ['T', 'U'], B: ['U']}", &claim)
                    .unwrap()
                    .unwrap_err();
                assert_eq!(reason.cause, Cause::RelationFalse);
                assert_eq!(reason.rule, Rule::TableOnly);
                assert_eq!(reason.target_token, Some(9));
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
                let missing = verdict("{A: ['T'], B: []}", &claim).unwrap().unwrap_err();
                assert_eq!(missing.target_token, Some(9));
                assert_eq!(missing.counterexample, vec![Position::Key(0)]);
            }
        }
    }

    // AC-0287 / #1729: no acceptance based on an identifier substring/prefix.
    #[test]
    fn bounded_predicates_reject_extra_prose() {
        for claim in [
            "A is terminal except on Tuesdays",
            "A is terminal and has no outgoing transitions unless enabled",
            "A is terminal and has no outgoing transitions and executes payments",
            "A is the only state that lists T if enabled",
            "A is the sole state that lists T while preventing it",
            "A is the only state that lists",
            "A is the only state that lists T and",
            "Unknown is the only state that lists T",
        ] {
            let source = if claim.starts_with("A is terminal") {
                "{A: [], B: []}"
            } else {
                "{A: ['T'], B: []}"
            };
            assert_eq!(
                verdict(source, claim).unwrap().unwrap_err().cause,
                Cause::OutsideGrammar,
                "{claim}"
            );
        }
        assert_eq!(verdict("{A: []}", "B says A is terminal"), None);
    }
}
