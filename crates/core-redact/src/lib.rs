//! Pure deterministic redaction shared by source recovery and higher tiers.
//!
//! Recognizers are shared, but callers retain distinct policies (SPEC-02,
//! ADR-0020). [`redact_text`] preserves the existing LLM payload substitutions;
//! [`is_token_shaped`] preserves the toolchain's conservative string detector.
//! Neither function parses source code or establishes that an input is safe.
//! The dependent AST policy must preserve literal types, inspect decoded values,
//! and record withheld source before expressions enter persistent rule facts.
//!
//! This crate performs no network, provider, filesystem, environment, or model
//! operations. Moving these helpers is groundwork for AC-0124, not completion
//! of the source-expression storage boundary.

/// Version of deterministic substitutions used for persistent embedding recipes.
pub const TEXT_POLICY_VERSION: &str = "redact-text@1";

use regex::Regex;
use std::sync::OnceLock;

const SENSITIVE_NAME_PATTERN: &str = r"api[_-]?key|access[_-]?token|auth[_-]?token|password|secret";

/// Category recognized by the shared deterministic secret patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    /// A private-key block.
    PrivateKey,
    /// A bearer authorization token.
    BearerToken,
    /// A provider-specific credential token.
    ProviderToken,
    /// An AWS access-key identifier.
    AwsAccessKey,
    /// A credential-assignment value in free text.
    CredentialAssignment,
}

fn patterns() -> &'static [(Regex, &'static str, SecretKind)] {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str, SecretKind)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            (
                Regex::new(
                    r"(?is)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
                )
                .unwrap(),
                "[REDACTED PRIVATE KEY]",
                SecretKind::PrivateKey,
            ),
            (
                Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{8,}").unwrap(),
                "Bearer [REDACTED]",
                SecretKind::BearerToken,
            ),
            (
                Regex::new(r"\b(?:github_pat_|gh[pousr]_|sk-)[A-Za-z0-9_-]{8,}").unwrap(),
                "[REDACTED TOKEN]",
                SecretKind::ProviderToken,
            ),
            (
                Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap(),
                "[REDACTED AWS ACCESS KEY]",
                SecretKind::AwsAccessKey,
            ),
            (
                Regex::new(&format!(
                    r"(?i)\b({SENSITIVE_NAME_PATTERN})\b{}",
                    r#"(\s*[:=]\s*[\"']?)([^\s\"',;}\]\[]{4,})"#,
                ))
                .unwrap(),
                "$1$2[REDACTED]",
                SecretKind::CredentialAssignment,
            ),
        ]
    })
}

/// Recognize categories in a decoded value without exposing matched text.
///
/// Each category appears at most once, in the free-text pattern order. Unlike
/// [`redact_text`], this function inspects the original input for every pattern,
/// so overlapping categories can both be reported. It does not apply the
/// distinct [`is_token_shaped`] policy or establish that an unmatched value is safe.
pub fn recognized_secrets(input: &str) -> Vec<SecretKind> {
    patterns()
        .iter()
        .filter_map(|(pattern, _, kind)| pattern.is_match(input).then_some(*kind))
        .collect()
}

/// Whether an entire source binding/property name is a recognized credential key.
///
/// Uses the same name alternatives as free-text credential assignment, anchored
/// to the whole name. Callers must establish the AST relationship to a value;
/// the function makes no inference about similarly named functions or variables.
pub fn is_sensitive_name(name: &str) -> bool {
    static NAME: OnceLock<Regex> = OnceLock::new();
    NAME.get_or_init(|| Regex::new(&format!(r"(?i)^(?:{SENSITIVE_NAME_PATTERN})$")).unwrap())
        .is_match(name)
}

/// Replace recognized secrets in free text and return the replacement count.
///
/// Patterns run in their established order, so the returned bytes and count
/// remain compatible with existing LLM previews and consent hashes. Existing
/// replacement markers are not counted again. This is a free-text policy, not
/// a source sanitizer: for example, `password=false` becomes
/// `password=[REDACTED]`. An AST-aware caller must retain that boolean's type
/// and value rather than applying this function blindly to an expression.
pub fn redact_text(input: &str) -> (String, usize) {
    let mut output = input.to_string();
    let mut count = 0;
    for (pattern, replacement, _) in patterns() {
        count += pattern.find_iter(&output).count();
        output = pattern.replace_all(&output, *replacement).into_owned();
    }
    (output, count)
}

/// Bound the serialized content of one direct UTF-8 source string, before and
/// after [`redact_text`], under [`TEXT_POLICY_VERSION`] `redact-text@1`.
///
/// Returns checked `6 * utf8_bytes`, excluding the two JSON string quotes. If a
/// serialized payload skeleton already contains an empty text string, add only
/// this content bound. The caller must establish that the eventual valid UTF-8
/// text has at most `utf8_bytes` bytes. Unknown policies and arithmetic overflow
/// return `None`; changing the policy requires re-proving this bound.
///
/// This applies to a string serialized directly once by `serde_json`, such as
/// `CompletionPayload.spans[i].text`. Source metadata, nested JSON strings,
/// prompts, provider transport encoding and whole-payload admission are excluded.
///
/// # Provenance argument for redact-text@1
///
/// Give each original UTF-8 byte six credits: JSON escaping costs at most six
/// bytes per original byte, including controls, quotes and backslashes. Each
/// private-key marker costs 22 ASCII bytes and replaces at least 52 original
/// bytes; a provider-token marker costs 16 for at least 11; an AWS marker costs
/// 25 for 20. Assignment replacement preserves captures `$1` and `$2` exactly
/// and spends at least four value characters (at least four original bytes) on
/// the ten-byte `[REDACTED]` marker. Preserved Unicode whitespace and captures
/// keep their original byte credits.
///
/// Bearer replacement retains provenance for its six canonical ASCII keyword
/// bytes, and spends the whitespace/token bytes (at least nine) on the eleven
/// bytes ` [REDACTED]`. This distinction matters across sequential passes:
/// `secret=Bearer abcdefgh` becomes `secret=Bearer [REDACTED]`, then
/// `secret=[REDACTED] [REDACTED]`. The final assignment can consume canonical
/// `Bearer`, which still carries the six original keyword bytes' credits.
///
/// Only the bracketed marker blocks are inert under later patterns: their
/// interiors contain no later token prefix or sensitive assignment name or
/// separator, and brackets stop token/value matches from traversing a block.
/// Newly exposed word boundaries can match remaining original bytes, but do
/// not reuse spent provenance. Thus surviving text, canonical keyword bytes
/// and inserted markers are paid for by disjoint original byte credits. No
/// assumption that redaction shrinks text, or that every replacement is inert,
/// is needed. The sixfold bound is sharp for a run of NUL characters.
pub fn direct_source_json_content_bound(utf8_bytes: usize) -> Option<usize> {
    if TEXT_POLICY_VERSION != "redact-text@1" {
        return None;
    }
    utf8_bytes.checked_mul(6)
}

/// Whether a string matches the toolchain's conservative token-shaped policy.
///
/// The value must contain at least 40 UTF-8 bytes, no whitespace, and only
/// ASCII letters, digits, or `+ / _ - = .`. This intentionally includes some
/// long business identifiers; callers must disclose withholding rather than
/// treating a detected value as proven secret or silently interpreting it.
/// This predicate does not replace text and does not detect every secret.
pub fn is_token_shaped(value: &str) -> bool {
    value.len() >= 40
        && !value.contains(char::is_whitespace)
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '_' | '-' | '=' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_recognition_reports_categories_without_changing_replacement_policy() {
        // AC-0124 groundwork: AST policy receives categories, never reverse
        // engineers replacement text or maintains independent secret regexes.
        assert_eq!(
            recognized_secrets("Bearer sk-abcdefgh1234"),
            vec![SecretKind::BearerToken, SecretKind::ProviderToken]
        );
        assert_eq!(
            redact_text("Bearer sk-abcdefgh1234"),
            ("Bearer [REDACTED]".into(), 1)
        );
        assert_eq!(
            recognized_secrets("password=synthetic-value"),
            vec![SecretKind::CredentialAssignment]
        );
        assert!(recognized_secrets("[REDACTED TOKEN]").is_empty());
        assert!(recognized_secrets("ordinary business message").is_empty());
        assert!(recognized_secrets(&"a".repeat(40)).is_empty());
    }

    #[test]
    fn sensitive_names_reuse_assignment_keys_with_exact_boundaries() {
        // AC-0124 groundwork: a proven property named password is sensitive;
        // passwordRequired and setPassword do not imply a credential value.
        for name in [
            "password",
            "SECRET",
            "api-key",
            "api_key",
            "apiKey",
            "accessToken",
            "auth_token",
        ] {
            assert!(is_sensitive_name(name));
            assert_eq!(redact_text(&format!("{name}=synthetic-value")).1, 1);
        }
        for name in [
            "passwordRequired",
            "setPassword",
            "secrets",
            "password ",
            "not-secret",
        ] {
            assert!(!is_sensitive_name(name));
        }
    }

    #[test]
    fn free_text_redaction_preserves_existing_payload_bytes_and_counts() {
        // AC-0124 groundwork: relocation must preserve the existing egress
        // policy before the dependent AST storage policy uses these helpers.
        let cases = [
            (
                "-----BEGIN RSA PRIVATE KEY-----\nsynthetic key\n-----END RSA PRIVATE KEY-----",
                "[REDACTED PRIVATE KEY]",
            ),
            ("Bearer abcdefgh1234", "Bearer [REDACTED]"),
            ("github_pat_abcdefgh1234", "[REDACTED TOKEN]"),
            ("ghp_abcdefgh1234", "[REDACTED TOKEN]"),
            ("sk-abcdefgh1234", "[REDACTED TOKEN]"),
            ("AKIA0000000000000000", "[REDACTED AWS ACCESS KEY]"),
            ("password=super-secret", "password=[REDACTED]"),
            ("api-key: 'synthetic-value'", "api-key: '[REDACTED]'"),
            ("ACCESS_TOKEN=synthetic-value", "ACCESS_TOKEN=[REDACTED]"),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_text(input), (expected.into(), 1));
        }

        // The bearer recognizer runs before the provider-token recognizer;
        // an overlapping token is replaced once, not counted twice.
        assert_eq!(
            redact_text("Bearer sk-abcdefgh1234\npassword=synthetic-value"),
            ("Bearer [REDACTED]\npassword=[REDACTED]".into(), 2)
        );
    }

    #[test]
    fn free_text_redaction_is_idempotent_without_changing_safe_text() {
        // AC-0124 groundwork: previewed output remains stable on re-use.
        let safe = "queue=orders\nif (options.enabled !== false) return 'not-ready';";
        assert_eq!(redact_text(safe), (safe.into(), 0));

        let first = redact_text(
            "-----BEGIN PRIVATE KEY-----\nsynthetic\n-----END PRIVATE KEY-----\nBearer abcdefgh1234\npassword=synthetic-value\nAKIA0000000000000000\nsk-abcdefgh1234",
        );
        assert_eq!(first.1, 5);
        assert_eq!(redact_text(&first.0), (first.0, 0));
    }

    #[test]
    fn free_text_assignment_policy_is_not_a_source_literal_policy() {
        // AC-0124 groundwork: compatibility is intentional; the dependent
        // AST sanitizer must preserve typed false rather than use this result.
        assert_eq!(
            redact_text("password=false"),
            ("password=[REDACTED]".into(), 1)
        );
        assert_eq!(redact_text("false"), ("false".into(), 0));
    }

    #[test]
    fn token_shape_preserves_toolchain_threshold_and_character_rules() {
        // AC-0124 groundwork and AC-0097 compatibility: no broadening or
        // weakening of the existing toolchain omission policy during reuse.
        assert!(!is_token_shaped(&"a".repeat(39)));
        assert!(is_token_shaped(&"a".repeat(40)));
        assert!(is_token_shaped(&format!("{}+/_-=.", "a".repeat(34))));
        for suffix in [" ", "\n", "\t", ":", "@", "é", "\u{2003}"] {
            assert!(!is_token_shaped(&format!("{}{suffix}", "a".repeat(40))));
        }
        assert!(!is_token_shaped(""));
        assert!(!is_token_shaped("false"));

        // Conservative false positives remain visible to the caller; do not
        // special-case a project's message names in shared recognition.
        assert!(is_token_shaped(
            "message.cannot-start-action-without-required-context"
        ));
    }

    fn assert_direct_source_content_bound(text: &str) {
        let bound = direct_source_json_content_bound(text.len()).unwrap();
        let original = serde_json::to_vec(text).unwrap();
        let redacted = serde_json::to_vec(&redact_text(text).0).unwrap();
        assert!(original.len() - 2 <= bound);
        assert!(redacted.len() - 2 <= bound);
    }

    #[test]
    fn direct_source_bound_covers_controls_unicode_and_json_escaping() {
        // #708: count UTF-8 bytes and one actual JSON encoding, not characters.
        assert_eq!(TEXT_POLICY_VERSION, "redact-text@1");
        let controls = (0_u8..=31).map(char::from).collect::<String>();
        assert_direct_source_content_bound(&controls);
        for text in [
            "",
            "\"\\/",
            "é漢🦀",
            "\u{7f}\u{85}\u{2028}\u{2029}",
            "Bearer\u{2003}abcdefgh",
            "ſecret\u{85}=\u{2003}'éééé'",
        ] {
            assert_direct_source_content_bound(text);
        }
    }

    #[test]
    fn direct_source_bound_covers_shortest_matches_and_cross_pass_expansion() {
        // #708: every expanding replacement is exercised through real redaction.
        for (text, expected) in [
            (
                "-----BEGIN PRIVATE KEY----------END PRIVATE KEY-----",
                "[REDACTED PRIVATE KEY]",
            ),
            ("Bearer abcdefgh", "Bearer [REDACTED]"),
            ("sk-abcdefgh", "[REDACTED TOKEN]"),
            ("ghp_abcdefgh", "[REDACTED TOKEN]"),
            ("github_pat_abcdefgh", "[REDACTED TOKEN]"),
            ("AKIA0000000000000000", "[REDACTED AWS ACCESS KEY]"),
            ("secret=aaaa", "secret=[REDACTED]"),
            ("secret=Bearer abcdefgh", "secret=[REDACTED] [REDACTED]"),
        ] {
            assert_eq!(redact_text(text).0, expected);
            assert_direct_source_content_bound(text);
        }
        assert_eq!(redact_text("secret=Bearer abcdefgh").1, 2);
        assert!(redact_text("secret=aaaa").0.len() > "secret=aaaa".len());
    }

    #[test]
    fn direct_source_bound_covers_mixed_repeated_sequential_redaction() {
        // #708: preserved whitespace/quotes, escaping and generated Bearer text mix.
        let corpus = concat!(
            "secret=Bearer abcdefgh; password\t=\n'aaaa'; ",
            "sk-abcdefgh/ghp_abcdefgh github_pat_abcdefgh AKIA0000000000000000\n",
            "-----BEGIN PRIVATE KEY-----\0\"\\🦀-----END PRIVATE KEY-----\n",
            "Bearer\u{2003}abcdefgh ſecret\u{85}=\u{2003}'éééé'\0\u{1f}\"\\\n",
        );
        for count in [1, 2, 8, 64] {
            assert_direct_source_content_bound(&corpus.repeat(count));
        }
    }

    #[test]
    fn direct_source_bound_is_sharp_for_control_runs_and_checked_at_usize_limit() {
        // #708: quotes are excluded; a single byte less cannot cover this source.
        let text = "\0".repeat(8192);
        let bound = direct_source_json_content_bound(text.len()).unwrap();
        assert_eq!(serde_json::to_vec(&text).unwrap().len() - 2, bound);
        assert_eq!(
            serde_json::to_vec(&redact_text(&text).0).unwrap().len() - 2,
            bound
        );
        assert!(serde_json::to_vec(&text).unwrap().len() - 2 > bound - 1);
        assert_eq!(direct_source_json_content_bound(0), Some(0));
        let largest = usize::MAX / 6;
        assert_eq!(direct_source_json_content_bound(largest), Some(largest * 6));
        assert_eq!(direct_source_json_content_bound(largest + 1), None);
        assert_eq!(direct_source_json_content_bound(usize::MAX), None);
    }
}
