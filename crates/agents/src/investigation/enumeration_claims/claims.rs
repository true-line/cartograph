//! Conservative checks over literal enumerations, not a natural-language truth oracle.
#[path = "literal.rs"]
mod literal;
use literal::Enumeration;

/// None means no literal table identifier binds the clause. False also covers
/// bound claims whose relation is outside the deliberately small grammar.
pub(super) fn check(source: &str, sentence: &str) -> Option<bool> {
    let enumeration = literal::parse(source)?;
    let mut applicable = false;
    // Separate coordinated independent clauses without losing target lists
    // joined by `or`. Do not infer a relation across clause boundaries.
    for clause in sentence.split(", and ") {
        if let Some(valid) = check_clause(&enumeration, clause) {
            if !valid {
                return Some(false);
            }
            applicable = true;
        }
    }
    applicable.then_some(true)
}

fn tokens(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .filter(|word| !word.is_empty())
        .collect()
}

fn check_clause(table: &Enumeration<'_>, text: &str) -> Option<bool> {
    let words = tokens(text);
    let lower: Vec<_> = words.iter().map(|word| word.to_ascii_lowercase()).collect();
    let quantifier = lower.iter().position(|word| {
        matches!(
            word.as_str(),
            "every" | "all" | "each" | "any" | "only" | "none" | "no" | "universal" | "sole"
        )
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
    // These qualifiers alter domains or negate predicates; the bounded grammar
    // does not resolve them. Never silently evaluate the unqualified relation.
    if lower.iter().any(|word| {
        matches!(
            word.as_str(),
            "except" | "unless" | "not" | "without" | "if"
        )
    }) {
        return Some(false);
    }
    let Enumeration::Table(entries) = table else {
        let Enumeration::Array(values) = table else {
            unreachable!()
        };
        // Bare-array predicates assert membership, never invent table keys.
        return Some(match lower.as_slice() {
            [quantifier, domain, verb, _]
                if matches!(quantifier.as_str(), "every" | "all" | "each")
                    && matches!(domain.as_str(), "entry" | "entries" | "value" | "values")
                    && matches!(verb.as_str(), "is" | "are") =>
            {
                values.iter().all(|value| *value == words[3])
            }
            [only, _, rest @ ..] if only == "only" && rest == ["is", "listed"] => {
                !values.is_empty() && values.iter().all(|value| *value == words[1])
            }
            _ => false,
        });
    };
    let keyword = lower[quantifier].as_str();
    let result = match keyword {
        "every" | "all" | "each" if quantifier == 0 => {
            let predicate = lower.iter().position(|word| {
                matches!(
                    word.as_str(),
                    "lists" | "list" | "allows" | "allow" | "permits" | "permit"
                )
            });
            match predicate {
                Some(predicate) if predicate > 1 => {
                    let domain = lower[1..predicate].join(" ");
                    let domain_is_all = matches!(
                        domain.as_str(),
                        "state"
                            | "states"
                            | "entry"
                            | "entries"
                            | "key"
                            | "keys"
                            | "state entry"
                            | "state entries"
                    );
                    domain_is_all
                        && target_list(&words[predicate + 1..]).is_some_and(|targets| {
                            targets.iter().all(|target| {
                                entries.iter().all(|(_, values)| values.contains(target))
                            })
                        })
                }
                _ => false,
            }
        }
        "only" if quantifier == 0 => {
            let key_index = if lower.get(1).is_some_and(|word| word == "the") {
                2
            } else {
                1
            };
            words
                .get(key_index)
                .and_then(|key| entries.iter().find(|(candidate, _)| candidate == key))
                .is_some_and(|(key, values)| {
                    let predicate = key_index
                        + 1
                        + usize::from(lower.get(key_index + 1).is_some_and(|word| word == "state"));
                    lower
                        .get(predicate)
                        .is_some_and(|word| matches!(word.as_str(), "lists" | "allows" | "permits"))
                        && target_list(&words[predicate + 1..]).is_some_and(|targets| {
                            targets.iter().all(|target| values.contains(target))
                                && entries
                                    .iter()
                                    .filter(|(_, values)| {
                                        targets.iter().any(|target| values.contains(target))
                                    })
                                    .all(|(candidate, _)| candidate == key)
                        })
                })
        }
        "no" if quantifier == 2 => entries
            .iter()
            .find(|(key, _)| Some(key) == words.first())
            .is_some_and(|(_, values)| {
                values.is_empty()
                    && matches!(lower[1].as_str(), "lists" | "permits" | "allows" | "has")
                    && matches!(
                        lower[3..].join(" ").as_str(),
                        "transitions"
                            | "further transitions"
                            | "outgoing transitions"
                            | "targets"
                            | "further targets"
                            | "outgoing targets"
                    )
            }),
        _ => false,
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
    const SOURCE: &str = include_str!("r3-transitions.txt");

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
