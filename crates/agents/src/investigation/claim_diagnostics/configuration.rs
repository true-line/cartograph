//! Syntactic configuration witnesses. Co-occurrence is never a binding.
use crate::investigation::enumeration_claims::subjects;

/// Recognizes narrowly supported configuration syntax in a cited source span.
/// Unknown syntax supplies no witness; it is not evaluated or inferred.
use crate::investigation::claim_diagnostics::ConfigurationFailure as Failure;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Witness {
    DefaultPath,
    DefaultIdentifier,
    ConfigurationAccess,
    DestructuredConfiguration,
    Assignment,
}

#[cfg(test)]
pub(super) fn binds(path: &str, source: &str, identifier: &str) -> bool {
    explain(path, source, identifier).is_ok()
}

/// A failed syntactic search is not proof that no runtime binding exists.
/// SubjectUnbound means absent from the tokenized cited span only.
pub(crate) fn explain(path: &str, source: &str, identifier: &str) -> Result<Witness, Failure> {
    let Some(tokens) = code_tokens(source) else {
        return Err(Failure::SourceTokenizationFailed);
    };
    let id_words = subjects::words(identifier);
    let stem = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .split('.')
        .next()
        .unwrap_or("");
    let path_words = subjects::words(stem);
    if path_words.first().is_some_and(|word| word == "default")
        && (path_words == id_words || path_words[1..] == id_words)
    {
        return Ok(Witness::DefaultPath);
    }
    if !tokens.contains(&identifier) {
        return Err(Failure::SubjectUnbound);
    }
    if id_words.first().is_some_and(|word| word == "default") {
        return Ok(Witness::DefaultIdentifier);
    }
    for (index, token) in tokens.iter().enumerate() {
        if *token != identifier {
            continue;
        }
        // Direct access: this.configService.catalogOptions.stockLocationStrategy.
        let mut start = index;
        while start >= 2 && tokens[start - 1] == "." && is_identifier(tokens[start - 2]) {
            start -= 2;
        }
        if start < index && config_path(&tokens[start..=index]) {
            return Ok(Witness::ConfigurationAccess);
        }
        // A single explicitly destructured binding: const { X } = config.path.
        if index >= 2
            && matches!(tokens[index - 2], "const" | "let" | "var")
            && tokens[index - 1] == "{"
            && tokens.get(index + 1..index + 3) == Some(&["}", "="][..])
        {
            let end = tokens[index + 3..]
                .iter()
                .position(|token| *token == ";")
                .map(|end| index + 3 + end);
            if let Some(end) = end
                && config_path(&tokens[index + 3..end])
            {
                return Ok(Witness::DestructuredConfiguration);
            }
        }
        // Explicit assignment of X (or new X) to a default-prefixed binding,
        // or to a member of a configuration path.
        let assignment = if index >= 1 && tokens[index - 1] == "new" {
            index - 1
        } else {
            index
        };
        if assignment >= 2 && matches!(tokens[assignment - 1], "=" | ":") {
            let lhs_end = assignment - 2;
            let mut lhs_start = lhs_end;
            while lhs_start >= 2
                && tokens[lhs_start - 1] == "."
                && is_identifier(tokens[lhs_start - 2])
            {
                lhs_start -= 2;
            }
            let rhs_end = if assignment < index {
                if tokens.get(index + 1..index + 3) != Some(&["(", ")"][..]) {
                    continue;
                }
                index + 3
            } else {
                index + 1
            };
            if !tokens
                .get(rhs_end)
                .is_some_and(|token| matches!(*token, ";" | "," | "}"))
            {
                continue;
            }
            if subjects::words(tokens[lhs_end])
                .first()
                .is_some_and(|word| word == "default")
                || (tokens[assignment - 1] == "=" && config_path(&tokens[lhs_start..=lhs_end]))
            {
                return Ok(Witness::Assignment);
            }
        }
    }
    Err(Failure::BindingNotRecognized)
}

fn is_identifier(token: &str) -> bool {
    subjects::identifiers(token).next() == Some(token)
}

fn config_path(tokens: &[&str]) -> bool {
    !tokens.is_empty()
        && tokens.len() % 2 == 1
        && tokens.iter().enumerate().all(|(index, token)| {
            if index % 2 == 0 {
                is_identifier(token)
            } else {
                *token == "."
            }
        })
        && tokens.iter().step_by(2).any(|token| {
            subjects::words(token)
                .iter()
                .any(|word| matches!(word.as_str(), "config" | "configuration"))
        })
}

fn code_tokens(mut source: &str) -> Option<Vec<&str>> {
    let mut result = Vec::new();
    while !source.is_empty() {
        source = source.trim_start();
        if source.is_empty() {
            break;
        }
        if let Some(comment) = source.strip_prefix("//") {
            source = comment.split_once('\n').map_or("", |(_, tail)| tail);
            continue;
        }
        if let Some(comment) = source.strip_prefix("/*") {
            source = comment.split_once("*/")?.1;
            continue;
        }
        let first = source.chars().next()?;
        if matches!(first, '\'' | '"' | '`') {
            // Ignore string/comment claims as configuration evidence. Escapes
            // are skipped solely to find the delimiter, never interpreted.
            let mut escaped = false;
            let mut end = None;
            for (index, ch) in source.char_indices().skip(1) {
                if escaped {
                    escaped = false;
                    continue;
                }
                if ch == '\\' {
                    escaped = true;
                    continue;
                }
                if ch == first {
                    end = Some(index + ch.len_utf8());
                    break;
                }
            }
            source = &source[end?..];
            // A sentinel prevents token adjacency across omitted literals.
            result.push("");
            continue;
        }
        let length = if first.is_alphabetic() || matches!(first, '_' | '$') {
            source
                .char_indices()
                .find(|(_, ch)| !(ch.is_alphanumeric() || matches!(ch, '_' | '$')))
                .map_or(source.len(), |(index, _)| index)
        } else {
            first.len_utf8()
        };
        result.push(&source[..length]);
        source = &source[length..];
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    // AC-0287 / #1707: distinguish absent source tokens from unsupported
    // binding syntax without changing the historical boolean decision.
    #[test]
    fn configuration_reason_paths() {
        for (path, source, id, expected) in [
            (
                "x.ts",
                "/* unfinished",
                "Strategy",
                Err(Failure::SourceTokenizationFailed),
            ),
            (
                "x.ts",
                "class Other {}",
                "Strategy",
                Err(Failure::SubjectUnbound),
            ),
            (
                "x.ts",
                "config.strategy = new Strategy(options);",
                "Strategy",
                Err(Failure::BindingNotRecognized),
            ),
            (
                "default-strategy.ts",
                "class Other {}",
                "Strategy",
                Ok(Witness::DefaultPath),
            ),
            (
                "x.ts",
                "class DefaultStrategy {}",
                "DefaultStrategy",
                Ok(Witness::DefaultIdentifier),
            ),
            (
                "x.ts",
                "this.config.strategy",
                "strategy",
                Ok(Witness::ConfigurationAccess),
            ),
            (
                "x.ts",
                "const { strategy } = config.options;",
                "strategy",
                Ok(Witness::DestructuredConfiguration),
            ),
            (
                "x.ts",
                "config.strategy = new Strategy();",
                "Strategy",
                Ok(Witness::Assignment),
            ),
        ] {
            assert_eq!(explain(path, source, id), expected);
            assert_eq!(binds(path, source, id), expected.is_ok());
        }
    }

    // AC-0287: real r3 configuration access form, with unrelated text refused.
    #[test]
    fn binds_destructuring_and_default_identity_without_cooccurrence() {
        assert!(binds(
            "stock-location.service.ts",
            "const { stockLocationStrategy } = this.configService.catalogOptions;",
            "stockLocationStrategy"
        ));
        assert!(binds(
            "default-stock-allocation-strategy.ts",
            "export class DefaultStockAllocationStrategy {}",
            "DefaultStockAllocationStrategy"
        ));
        assert!(binds(
            "config.ts",
            "config.catalog.strategy = new MultiChannelStockLocationStrategy();",
            "MultiChannelStockLocationStrategy"
        ));
        assert!(binds(
            "config.ts",
            "const defaultStrategy = new Strategy();",
            "Strategy"
        ));
        for source in [
            "const config = {}; class Strategy {}",
            "// config.strategy = new Strategy();\nclass Strategy {}",
            "const description = 'config.strategy = new Strategy()'; class Strategy {}",
            "const { Other } = config.options; class Strategy {}",
            "const value = new Strategy(); const config = {};",
            "config.strategy = unrelated(Strategy);",
            "config.strategy = new Strategy() || fallback;",
            "config.strategy = Strategy.factory();",
            "const defaultStrategy = Strategy ? Other : Third;",
            "config.strategy = new Strategy(options);",
            "/* unclosed config.strategy = Strategy",
        ] {
            assert!(!binds("strategy.ts", source, "Strategy"), "{source}");
        }
        assert!(!binds(
            "stock-location.service.ts",
            "const { stockLocationStrategy } = this.configService.catalogOptions;",
            "MultiChannelStockLocationStrategy"
        ));
        assert!(!binds(
            "multi-channel-stock-location-strategy.ts",
            "class MultiChannelStockLocationStrategy {}",
            "MultiChannelStockLocationStrategy"
        ));
    }
}
