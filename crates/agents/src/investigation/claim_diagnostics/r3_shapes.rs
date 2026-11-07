//! AC-0287 / #1707: exact r3 refusal forms and their accepted controls.
use super::*;
const SOURCE: &str = include_str!("../enumeration_claims/r3-transitions.txt");

fn positions(symbol: &str) -> Vec<Position> {
    // Independently read fixture row/element indexes; no production name lookup.
    let encoded = match symbol {
        "C" => "1:0 2:1 3:3 4:1 5:4 6:2 7:2 8:1 9:0 11:5 K12",
        "A" => "1:1 2:0 K3",
        "P" => "3:1 4:0 K5 10:1 11:1",
        "K" => "K0",
        "M" => "4:2 5:5 6:3 7:3 8:2 K10",
        _ => panic!("unexpected fixture symbol"),
    };
    encoded
        .split_whitespace()
        .map(|item| {
            if let Some(key) = item.strip_prefix('K') {
                Position::Key(key.parse().unwrap())
            } else {
                let (key, value) = item.split_once(':').unwrap();
                Position::Value {
                    key: Some(key.parse().unwrap()),
                    value: value.parse().unwrap(),
                }
            }
        })
        .collect()
}

#[test]
fn exact_r3_shapes_and_source_positions() {
    for (sentence, expected, cause) in [
        (
            "Order lifecycle allows cancellation from every active state except Cancelled itself",
            "W W allows W W every W state except C W",
            Some(Cause::QualifierPresent(Qualifier::Except)),
        ),
        (
            "The transition table shows only the ArrangingPayment state can begin payment authorization or settlement, and that delivery and shipping states branch from PaymentSettled.",
            "the W W W only the A state W W W W or W and W W and W states W W P",
            Some(Cause::OutsideGrammar),
        ),
        (
            "Every pre-cancellation state lists Cancelled as a permitted target, indicating cancellation is a universal exit available throughout the pre-cancelled lifecycle, while Cancelled is terminal in this table.",
            "every W W state lists C W W W W W W is W universal W W W the W W W W C is W W W W",
            Some(Cause::OutsideGrammar),
        ),
        (
            "Each state entry lists only the target states it allows, and Cancelled permits no further transitions.",
            "each state entry lists only the W states W allows and C permits no further transitions",
            None,
        ),
        (
            "Cancelled lists no further transitions",
            "C lists no further transitions",
            None,
        ),
        (
            "Cancelled is terminal and has no outgoing transitions",
            "C is W and has no outgoing transitions",
            Some(Cause::OutsideGrammar),
        ),
        (
            "Every state except Created and Modifying lists Cancelled",
            "every state except K and M lists C",
            Some(Cause::QualifierPresent(Qualifier::Except)),
        ),
        (
            "Each state entry lists only the target states it allows",
            "each state entry lists only the W states W allows",
            None,
        ),
    ] {
        let (_, actual) = enumeration::describe(SOURCE, sentence, MAX_MANIFEST_BYTES).unwrap();
        let tokens: Vec<_> = expected
            .split_whitespace()
            .map(|token| match token {
                "W" => ShapeToken::Word,
                "C" | "A" | "P" | "K" | "M" => ShapeToken::Identifier(positions(token)),
                word => ShapeToken::Vocabulary(GrammarWord::from_word(word).unwrap()),
            })
            .collect();
        let matched = tokens
            .iter()
            .filter(|token| matches!(token, ShapeToken::Identifier(_)))
            .count();
        assert_eq!(
            actual,
            Shape {
                tokens,
                matched_identifiers: matched,
                omitted_tokens: 0,
                omitted_positions: 0
            },
            "{sentence}"
        );
        assert_eq!(
            explain_enumeration(SOURCE, sentence, MAX_MANIFEST_BYTES)
                .map(|diagnostic| diagnostic.explanation.cause),
            cause,
            "{sentence}"
        );
    }
}

// AC-0287 / #1707: failing-clause metadata uses full-sentence token indexes.
#[test]
fn compound_claim_points_to_the_failing_quantifier() {
    let diagnostic = explain_enumeration(
        "{A: ['Target'], B: ['Target']}",
        "Every state lists Target, and only A lists Target",
        MAX_MANIFEST_BYTES,
    )
    .unwrap();
    assert_eq!(diagnostic.quantifier, Some(GrammarWord::Only));
    assert_eq!(diagnostic.explanation.quantifier_token, Some(5));
    assert_eq!(diagnostic.explanation.target_token, Some(8));
    assert_eq!(diagnostic.explanation.cause, Cause::RelationFalse);
}
