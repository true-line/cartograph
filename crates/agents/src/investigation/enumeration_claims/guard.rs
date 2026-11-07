//! Finding-local claim checks. The host must separately authenticate citations,
//! charge workspace, and bind the returned text-free location to the refusal.
#[path = "claims.rs"]
mod claims;
#[path = "configuration.rs"]
mod configuration;
#[path = "subjects.rs"]
mod subjects;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Reason {
    Enumeration,
    Configuration,
}

/// Spans must already be restricted to this finding's admitted citation IDs.
/// Limitations are deliberately absent from this interface.
pub(super) fn finding(
    title: &str,
    statement: &str,
    spans: &[(&str, &str)],
) -> Option<(bool, usize, Reason)> {
    for (is_title, text) in [(true, title), (false, statement)] {
        for (index, sentence) in sentences(text).enumerate() {
            if spans
                .iter()
                .any(|(_, source)| claims::check(source, sentence) == Some(false))
            {
                return Some((is_title, index + 1, Reason::Enumeration));
            }
            if unverified_configuration(sentence, statement, spans, is_title) {
                return Some((is_title, index + 1, Reason::Configuration));
            }
        }
    }
    None
}

fn sentences(text: &str) -> impl Iterator<Item = &str> {
    let mut terminal = false;
    text.split(move |ch: char| {
        if ch.is_whitespace() {
            let split = terminal;
            terminal = false;
            split
        } else {
            terminal = matches!(ch, '.' | '!' | '?' | '。' | '！' | '？');
            false
        }
    })
    .filter(|part| !part.trim().is_empty())
}

fn unverified_configuration(
    sentence: &str,
    statement: &str,
    spans: &[(&str, &str)],
    title: bool,
) -> bool {
    let tokens: Vec<_> = subjects::identifiers(sentence).collect();
    let candidates: std::collections::BTreeSet<_> = subjects::identifiers(statement)
        .chain(
            spans
                .iter()
                .flat_map(|(_, source)| subjects::identifiers(source)),
        )
        .filter(|candidate| subjects::words(candidate).len() > 1)
        .collect();
    for (index, token) in tokens.iter().enumerate() {
        if !matches!(
            token.to_ascii_lowercase().as_str(),
            "default" | "defaulting" | "configured" | "registered"
        ) {
            continue;
        }
        let mut after = index + 1;
        while tokens.get(after).is_some_and(|token| {
            matches!(
                token.to_ascii_lowercase().as_str(),
                "to" | "the" | "a" | "an"
            )
        }) {
            after += 1;
        }
        let exact = tokens
            .get(after)
            .filter(|token| candidates.contains(**token))
            .copied();
        let subject = exact
            .or_else(|| {
                if !title {
                    return None;
                }
                // Longest word run first; ambiguity at that run never falls back
                // to an even shorter, less specific subject.
                let tail = tokens[after..].join(" ");
                let words = subjects::words(&tail);
                for count in (1..=words.len()).rev() {
                    let phrase = words[..count].join(" ");
                    let matching: Vec<_> = candidates
                        .iter()
                        .copied()
                        .filter(|candidate| subjects::words(candidate).starts_with(&words[..count]))
                        .collect();
                    if !matching.is_empty() {
                        return subjects::subject(&phrase, matching.into_iter());
                    }
                }
                None
            })
            .or_else(|| {
                // Explicit subject-before-predicate form: X is [the] default.
                let before = &tokens[..index];
                match before {
                    [.., subject, "is"] | [.., subject, "is", "the"]
                        if candidates.contains(subject) =>
                    {
                        Some(*subject)
                    }
                    _ => None,
                }
            });
        if let Some(subject) = subject
            && !spans
                .iter()
                .any(|(path, source)| configuration::binds(path, source, subject))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    // AC-0287 / delegated #1698 scope: single-word names remain not applicable.
    #[test]
    fn single_word_configuration_subject_is_not_applicable() {
        assert_eq!(
            finding(
                "Cache defaulting to Redis",
                "Redis stores cached values.",
                &[("cache.ts", "class Redis {}")],
            ),
            None
        );
    }
    // AC-0287: no subject binding is borrowed from a different finding.
    #[test]
    fn title_default_binding_is_finding_local() {
        let body = "The supplied MultiChannelStockLocationStrategy.forAllocation allocates stock.";
        let spans = [(
            "multi-channel-stock-location-strategy.ts",
            "forAllocation() { return []; }",
        )];
        assert_eq!(
            finding(
                "Allocation defaulting to multi-channel allocation",
                body,
                &spans
            ),
            Some((true, 1, Reason::Configuration))
        );
        assert_eq!(
            finding("Allocation defaulting to unrelated strategy", body, &spans),
            None
        );
        assert_eq!(
            finding(
                "Allocation defaulting to multi-channel allocation",
                "MultiChannelStockLocationStrategy and MultiChannelOther are described.",
                &spans
            ),
            None
        );
        assert_eq!(
            finding(
                "Configured allocation",
                "The configured catalog stockLocationStrategy chooses locations.",
                &[(
                    "service.ts",
                    "const { stockLocationStrategy } = this.configService.catalogOptions;"
                )]
            ),
            None
        );
    }
}

#[cfg(test)]
#[path = "r3_golden.rs"]
mod r3_golden;
