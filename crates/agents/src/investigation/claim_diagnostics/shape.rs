//! Text-free projection of the detector's actual token stream. The caller supplies
//! the remaining retention budget; exhausting it never changes detection.
use super::{Enumeration, Position, tokens};

use crate::investigation::claim_diagnostics::{GrammarWord, Shape, ShapeToken as Token};

fn vocabulary(word: &str) -> Option<GrammarWord> {
    GrammarWord::from_word(word)
}

fn positions(table: &Enumeration<'_>, word: &str, mut visit: impl FnMut(Position)) {
    match table {
        Enumeration::Array(values) => {
            for (value, name) in values.iter().enumerate() {
                if *name == word {
                    visit(Position::Value { key: None, value });
                }
            }
        }
        Enumeration::Table(entries) => {
            for (key, (name, values)) in entries.iter().enumerate() {
                if *name == word {
                    visit(Position::Key(key));
                }
                for (value, name) in values.iter().enumerate() {
                    if *name == word {
                        visit(Position::Value {
                            key: Some(key),
                            value,
                        });
                    }
                }
            }
        }
    }
}

/// Budgets count retained tokens and positions, independently. Keep counting
/// omitted data after either budget is exhausted. Positions use case-sensitive
/// equality, just like the detector, even when a name is also a grammar word.
pub(super) fn project(
    table: &Enumeration<'_>,
    sentence: &str,
    token_budget: usize,
    position_budget: usize,
) -> Shape {
    let mut shape = Shape {
        tokens: vec![],
        omitted_tokens: 0,
        omitted_positions: 0,
        matched_identifiers: 0,
    };
    let mut remaining_positions = position_budget;
    for word in tokens(sentence) {
        let retain = shape.tokens.len() < token_budget;
        let mut matched = false;
        let mut retained = Vec::new();
        positions(table, word, |position| {
            matched = true;
            if retain && remaining_positions > 0 {
                retained.push(position);
                remaining_positions -= 1;
            } else {
                shape.omitted_positions += 1;
            }
        });
        shape.matched_identifiers += usize::from(matched);
        if !retain {
            shape.omitted_tokens += 1;
            continue;
        }
        shape.tokens.push(if matched {
            Token::Identifier(retained)
        } else if let Some(fixed) = vocabulary(word) {
            Token::Vocabulary(fixed)
        } else if word.chars().all(|c| c.is_numeric()) {
            Token::Number
        } else {
            Token::Word
        });
    }
    shape
}

#[cfg(test)]
mod tests {
    use super::super::literal;
    use super::*;

    // AC-0287 / #1707: retained positions let a reviewer recover both repeated
    // target occurrences without storing their names or arbitrary prose.
    #[test]
    fn exact_shape_and_reconstruction() {
        let table = literal::parse("{A: ['Target'], B: ['Target']}").unwrap();
        let shape = project(&table, "Only the A state lists Target", 32, 32);
        assert_eq!(
            shape.tokens,
            vec![
                Token::Vocabulary(GrammarWord::Only),
                Token::Vocabulary(GrammarWord::The),
                Token::Identifier(vec![Position::Key(0)]),
                Token::Vocabulary(GrammarWord::State),
                Token::Vocabulary(GrammarWord::Lists),
                Token::Identifier(vec![
                    Position::Value {
                        key: Some(0),
                        value: 0
                    },
                    Position::Value {
                        key: Some(1),
                        value: 0
                    },
                ]),
            ]
        );
        assert_eq!(shape.matched_identifiers, 2);
        let reason = super::super::explain(
            "{A: ['Target'], B: ['Target']}",
            "Only the A state lists Target",
        )
        .unwrap()
        .unwrap_err();
        let Enumeration::Table(entries) = table else {
            panic!()
        };
        let Position::Key(other) = reason.counterexample[0] else {
            panic!()
        };
        let Position::Value {
            key: Some(row),
            value,
        } = reason.counterexample[1]
        else {
            panic!()
        };
        assert_eq!(other, row);
        assert_ne!(other, 0);
        assert!(entries[0].1.contains(&entries[row].1[value]));
    }

    // AC-0287 / #1707: tokenization discards punctuation; unknown Unicode and
    // malicious prose remain classes, and truncation cannot hide matched counts.
    #[test]
    fn privacy_and_budget_exhaustion() {
        let table = literal::parse("['Secret', 'Secret']").unwrap();
        let full = project(&table, "ALL entries: Secret 私密 123", 10, 10);
        assert_eq!(
            full.tokens,
            vec![
                Token::Vocabulary(GrammarWord::All),
                Token::Vocabulary(GrammarWord::Entries),
                Token::Identifier(vec![
                    Position::Value {
                        key: None,
                        value: 0
                    },
                    Position::Value {
                        key: None,
                        value: 1
                    }
                ]),
                Token::Word,
                Token::Number
            ]
        );
        let bounded = project(&table, "Secret Secret", 1, 1);
        assert_eq!(bounded.matched_identifiers, 2);
        assert_eq!(bounded.omitted_tokens, 1);
        assert_eq!(bounded.omitted_positions, 3);
        assert_eq!(
            bounded.tokens,
            vec![Token::Identifier(vec![Position::Value {
                key: None,
                value: 0
            }])]
        );
    }
}
