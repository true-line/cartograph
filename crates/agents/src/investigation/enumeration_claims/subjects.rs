//! Exact word-prefix resolution for configuration assertions; no synonyms.
use std::collections::BTreeSet;

pub(crate) fn words(identifier: &str) -> Vec<String> {
    let chars: Vec<_> = identifier.chars().collect();
    let mut result = Vec::new();
    let mut word = String::new();
    for (index, &ch) in chars.iter().enumerate() {
        if !ch.is_alphanumeric() {
            if !word.is_empty() {
                result.push(std::mem::take(&mut word));
            }
            continue;
        }
        let boundary = ch.is_uppercase()
            && index > 0
            && (chars[index - 1].is_lowercase()
                || (chars[index - 1].is_uppercase()
                    && chars.get(index + 1).is_some_and(|next| next.is_lowercase())));
        if boundary && !word.is_empty() {
            result.push(std::mem::take(&mut word));
        }
        word.extend(ch.to_lowercase());
    }
    if !word.is_empty() {
        result.push(word);
    }
    result
}

pub(crate) fn identifiers(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .filter(|part| {
            part.chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        })
}

/// The caller supplies only this finding's statement and cited spans. Returned
/// identifiers are borrowed; neither another finding nor a source outside the
/// citation set can disambiguate a subject.
pub(crate) fn subject<'a>(
    phrase: &str,
    candidates: impl Iterator<Item = &'a str>,
) -> Option<&'a str> {
    let phrase = words(phrase);
    let mut matches = BTreeSet::new();
    for candidate in candidates {
        let candidate_words = words(candidate);
        let prefix_matches = !phrase.is_empty() && candidate_words.starts_with(&phrase);
        // The contiguous run starts at the identifier's first word. A partial
        // word never matches, and suffixes never become substitute subjects.
        if prefix_matches {
            matches.insert(candidate);
        }
    }
    (matches.len() == 1).then(|| *matches.first().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    // AC-0287 / Decision #1697: approved finding-local unique prefix binding.
    #[test]
    fn unique_prefix_is_anchored_and_never_stemmed() {
        assert_eq!(
            subject(
                "multi-channel",
                ["MultiChannelStockLocationStrategy"].into_iter()
            ),
            Some("MultiChannelStockLocationStrategy")
        );
        assert_eq!(
            subject(
                "channel allocation",
                ["MultiChannelStockLocationStrategy"].into_iter()
            ),
            None
        );
        assert_eq!(
            subject(
                "multi-channel",
                ["MultiChannelStockLocationStrategy", "MultiChannelOther"].into_iter()
            ),
            None
        );
        assert_eq!(
            subject("allocating", ["AllocationStrategy"].into_iter()),
            None
        );
        assert_eq!(words("HTTPConfigPath"), ["http", "config", "path"]);
    }
}
