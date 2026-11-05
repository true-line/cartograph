//! Deterministic authored-field shape check, not a semantic truth classifier.
use super::*;

pub(super) fn has_placeholder(finish: &StagedFinish) -> bool {
    let InvestigationAction::Finish {
        findings,
        limitations,
        ..
    } = &finish.action
    else {
        return false;
    };
    limitations.iter().any(|s| placeholder(s))
        || findings.iter().any(|f| {
            placeholder(&f.title)
                || placeholder(&f.statement)
                || f.limitations.iter().any(|s| placeholder(s))
        })
}

fn placeholder(value: &str) -> bool {
    let value = value.trim();
    // Empty labels and punctuation-only fields cannot express a limitation.
    // A colon inside a sentence remains ordinary prose.
    if !value.chars().any(char::is_alphanumeric)
        || value.strip_suffix(':').is_some_and(|label| {
            !label.is_empty()
                && label
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
        })
    {
        return true;
    }
    let identifier = |s: &str| {
        let mut parts = s.split('_');
        let part = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric());
        parts.next().is_some_and(part) && parts.clone().next().is_some() && parts.all(part)
    };
    if !value.chars().any(char::is_whitespace) && identifier(value) {
        return true;
    }
    if value
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|token| {
            identifier(token)
                && token
                    .split('_')
                    .any(|part| part.eq_ignore_ascii_case("placeholder"))
        })
    {
        return true;
    }
    // Recognize leaked closing syntax only when unbalanced. Balanced JSON in
    // otherwise ordinary prose is allowed; this is not a JSON parser.
    let tail = value.trim_end_matches(['}', ']', '"', ',']);
    let suffix = &value[tail.len()..];
    suffix.len() >= 2
        && ((suffix.contains('}') && value.matches('}').count() > value.matches('{').count())
            || (suffix.contains(']') && value.matches(']').count() > value.matches('[').count())
            || (suffix.contains('"') && !value.matches('"').count().is_multiple_of(2)))
}

#[cfg(test)]
mod tests {
    use super::*;
    // AC-0566: deterministic refusal recognizes placeholders, not code mentions.
    #[test]
    fn analyst20_placeholder_shape_preserves_prose_and_code_mentions() {
        // All seven malformed authored fields from retained Flint call 015,
        // including repeated fields; kept verbatim, with no live provider use.
        // Retained evidence-boundaries and all-acquired limitations remain prose.
        for text in [
            "The evidence primarily covers error handling for rate limits but doesn't fully document the successful creation path or other edge cases.",
            "The evidence shows implementation details but does not cover all possible scenarios for creating new aliases, such as when the alias creation is successful or when other status codes are encountered.",
            "The implementation does not handle rate limit details beyond the error message (e.g., no retry logic or waiting period implementation).",
            "The extension's UI does not provide visual feedback for rate limits other than the error message.",
            "The implementation uses rate limit checking at runtime (429 status code), not a static feature.",
            "The error message 'Rate limit exceeded...' is hardcoded in multiple places, not dynamically generated.",
        ] {
            assert!(!placeholder(text), "rejected retained prose {text}");
        }
        for text in [
            "outcome_state_placeholder_must_not_be_used_here_but_outcome_must_be_supported_or_insufficient_evidence",
            "action_placeholder_must_not_be_used_here_but_action_must_be_finish}],",
            "statement_title_note_placeholder_must_not_be_used_here_but_title_must_be_short_and_nonblank",
            "statement_title_placeholder_must_not_be_used_here_but_title_must_be_short_and_nonblank",
            "statement_title_placeholder_must_not_be_used_here_but_title_must_be_short_and_nonblank",
            "statement_title_placeholder_must_not_be_used_here_but_title_must_be_short_and_nonblank",
            "statement_title_placeholder_must_not_be_used_here_but_title_must_be_short_and_nonblank",
        ] {
            assert!(placeholder(text), "missed retained field {text}");
        }
        for text in [
            "outcome_state_placeholder_must_not_be_used_here_but_outcome_must_be_supported_or_insufficient_evidence",
            "action_placeholder_must_not_be_used_here_but_action_must_be_finish}],",
            "statement_title_(note_)placeholder_must_not_be_used_here_but_title_must_be_short_and_nonblank",
            "unfinished prose}]",
            "unfinished prose\"}",
            "unfinished prose],",
            "schema_token",
        ] {
            assert!(placeholder(text), "missed {text}");
        }
        for text in [
            "The client invokes createCustomAlias for a custom alias.",
            "The client uses API_ROUTE.NEW_ALIAS and returns {} when appropriate.",
            "Only the selected source was inspected.",
            "The response can contain {}",
            "The JSON value is [1,2]",
            "Limitations",
            "The placeholder is described in ordinary prose.",
            "The field foo_bar is an identifier.",
        ] {
            assert!(!placeholder(text), "false positive {text}");
        }
    }
    // AC-0566: Flint run 2 emitted a bare label; no semantic auditor needed.
    #[test]
    fn analyst20_prose_refuses_bare_labels_but_preserves_colon_sentences() {
        for text in [
            "statement:",
            "  statement:  ",
            "Note:",
            "source_limit:",
            ":",
            "...",
            "{}",
            "—",
        ] {
            assert!(placeholder(text), "accepted empty authored field {text}");
        }
        for text in [
            "Note: only the client was inspected.",
            "HTTP status: 201",
            "The route is API_ROUTE.NEW_ALIAS.",
            "The response is {}.",
            "Limitations",
        ] {
            assert!(!placeholder(text), "rejected ordinary prose {text}");
        }
    }
    #[test]
    fn analyst20_prose_feedback_is_closed_and_historical_bytes_stay_absent() {
        let feedback = RefusalFeedback {
            schema_version: 3,
            prose_correction: Some(ProseCorrection::RewriteSchemaPlaceholdersAsPlainProse),
            citation_correction: None,
            unmet: vec![],
        };
        let bytes = feedback.bytes().unwrap();
        assert_eq!(
            serde_json::from_slice::<RefusalFeedback>(&bytes).unwrap(),
            feedback
        );
        // The existing schema-v3 reservation uses the placeholder variant as its bound.
        let mut replay = feedback.clone();
        replay.prose_correction = Some(ProseCorrection::ParaphraseWithoutQuotingSource);
        assert!(replay.bytes().unwrap().len() <= bytes.len());
        let mut forged = feedback.clone();
        forged.schema_version = 1;
        assert!(forged.bytes().is_err());
        forged = feedback.clone();
        forged.prose_correction = None;
        assert!(forged.bytes().is_err());
        assert!(
            serde_json::from_str::<RefusalFeedback>(
                r#"{"schema_version":3,"prose_correction":"arbitrary_model_text","unmet":[]}"#
            )
            .is_err()
        );
        let historical = r#"{"schema_version":1,"unmet":[{"obligation_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","kind":"source","reason":"not_attempted"}]}"#;
        let old: RefusalFeedback = serde_json::from_str(historical).unwrap();
        assert_eq!(old.bytes().unwrap(), historical.as_bytes());
    }
}
