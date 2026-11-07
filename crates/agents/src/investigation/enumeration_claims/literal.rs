//! Restricted literal reader. Never evaluates source or searches inside a larger expression.
#[derive(Debug, PartialEq)]
pub(super) enum Enumeration<'a> {
    Array(Vec<&'a str>),
    Table(Vec<(&'a str, Vec<&'a str>)>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Token<'a> {
    Word(&'a str),
    String(&'a str),
    Punct(u8),
}

struct Reader<'a> {
    rest: &'a str,
}

impl<'a> Reader<'a> {
    fn next(&mut self) -> Option<Token<'a>> {
        loop {
            self.rest = self.rest.trim_start();
            if let Some(comment) = self.rest.strip_prefix("//") {
                self.rest = comment.split_once('\n').map_or("", |(_, tail)| tail);
            } else if let Some(comment) = self.rest.strip_prefix("/*") {
                self.rest = comment.split_once("*/")?.1;
            } else {
                break;
            }
        }
        let first = *self.rest.as_bytes().first()?;
        if first == b'\'' || first == b'"' {
            let body = &self.rest[1..];
            let end = body.bytes().position(|b| b == first)?;
            let value = &body[..end];
            // Escaped strings require language-specific interpretation; not literal identifiers.
            if value.contains(['\\', '\n', '\r']) {
                return None;
            }
            self.rest = &body[end + 1..];
            return Some(Token::String(value));
        }
        if first.is_ascii_alphabetic() || matches!(first, b'_' | b'$') {
            let end = self
                .rest
                .bytes()
                .position(|b| !b.is_ascii_alphanumeric() && !matches!(b, b'_' | b'$'))
                .unwrap_or(self.rest.len());
            let word = &self.rest[..end];
            self.rest = &self.rest[end..];
            return Some(Token::Word(word));
        }
        // ASCII punctuation only: slicing never splits a UTF-8 scalar.
        if b"{}[]:,;".contains(&first) {
            self.rest = &self.rest[1..];
            Some(Token::Punct(first))
        } else {
            None
        }
    }

    fn punct(&mut self, expected: u8) -> Option<()> {
        (self.next()? == Token::Punct(expected)).then_some(())
    }

    fn array(&mut self) -> Option<Vec<&'a str>> {
        let mut values = Vec::new();
        loop {
            match self.next()? {
                Token::Punct(b']') => return Some(values),
                Token::String(value) => values.push(value),
                _ => return None,
            }
            match self.next()? {
                Token::Punct(b']') => return Some(values),
                Token::Punct(b',') => (),
                _ => return None,
            }
        }
    }

    fn table(&mut self) -> Option<Enumeration<'a>> {
        let mut entries = Vec::new();
        loop {
            let key = match self.next()? {
                Token::Punct(b'}') => return Some(Enumeration::Table(entries)),
                Token::Word(key) | Token::String(key) => key,
                _ => return None,
            };
            if entries.iter().any(|(existing, _)| *existing == key) {
                return None;
            }
            self.punct(b':')?;
            let values = match self.next()? {
                Token::Punct(b'[') => self.array()?,
                Token::Punct(b'{') => {
                    match self.next()? {
                        Token::Word(_) | Token::String(_) => (),
                        _ => return None,
                    }
                    self.punct(b':')?;
                    self.punct(b'[')?;
                    let values = self.array()?;
                    match self.next()? {
                        Token::Punct(b',') => self.punct(b'}')?,
                        Token::Punct(b'}') => (),
                        _ => return None,
                    }
                    values
                }
                _ => return None,
            };
            entries.push((key, values));
            match self.next()? {
                Token::Punct(b'}') => return Some(Enumeration::Table(entries)),
                Token::Punct(b',') => (),
                _ => return None,
            }
        }
    }
}

pub(super) fn parse(source: &str) -> Option<Enumeration<'_>> {
    let mut reader = Reader { rest: source };
    let mut first = reader.next()?;
    // Captured property spans commonly start at `transitions: { ... }`.
    if matches!(first, Token::Word(_) | Token::String(_)) {
        reader.punct(b':')?;
        first = reader.next()?;
    }
    let result = match first {
        Token::Punct(b'[') => Enumeration::Array(reader.array()?),
        Token::Punct(b'{') => reader.table()?,
        _ => return None,
    };
    // Consume trivia but reject all trailing expressions and unterminated trivia.
    let tail = reader.rest.trim();
    if !tail.is_empty() {
        return None;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-0287 / #1698: real captured source, not a reconstructed transition table.
    #[test]
    fn r3_transition_literal_preserves_all_entries() {
        let Enumeration::Table(entries) = parse(include_str!("r3-transitions.txt")).unwrap() else {
            panic!("expected table");
        };
        assert_eq!(entries.len(), 13);
        assert_eq!(entries[0], ("Created", vec!["AddingItems", "Draft"]));
        assert_eq!(entries.last(), Some(&("Cancelled", vec![])));
        assert!(
            !entries
                .iter()
                .find(|(key, _)| *key == "Modifying")
                .unwrap()
                .1
                .contains(&"Cancelled")
        );
    }

    #[test]
    fn literal_shapes_and_dynamic_rejection() {
        assert_eq!(
            parse("['A', 'B',]"),
            Some(Enumeration::Array(vec!["A", "B"]))
        );
        assert_eq!(
            parse("{ A: [/* comment */ 'B'], 'B': [] }"),
            Some(Enumeration::Table(vec![("A", vec!["B"]), ("B", vec![])]))
        );
        for text in [
            "{A: [...targets]}",
            "{A: [name]}",
            "{[key]: []}",
            "{A: [], A: ['B']}",
            "{A: {to: [], extra: []}}",
            "{A: []} || other",
            "{A: ['B']} /* unterminated",
            "{A: [`B`]}",
            "{A: ['B\\n']}",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }
}
