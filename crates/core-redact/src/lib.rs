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
///
/// `redact-text@2` (#1753) widens credential assignment over `redact-text@1`:
/// a name only has to *end* in a credential word (`DB_PASSWORD`, `clientSecret`,
/// `AWS_SECRET_ACCESS_KEY`), the key may be quoted (`"password": "…"`), `=>` is
/// a separator, a quoted value is replaced through its closing quote, and
/// `access_key`, `secret_key`, `private_key` and `passwd` join the credential
/// words. Everything bound to the previous version (embedding
/// cache keys, receipt source sizes, consent fingerprints) is recomputed.
pub const TEXT_POLICY_VERSION: &str = "redact-text@2";

mod env_values;
mod source_size;
pub use env_values::{env_secret_reason, is_env_template, without_userinfo};
pub use source_size::{DirectSourceJsonContentSizes, measure_direct_source_json_content};

use regex::Regex;
use std::sync::OnceLock;

/// Credential words. A free-text assignment name must end in one of them; a
/// source binding name must equal one of them ([`is_sensitive_name`]). Generic
/// `token` and `key` suffixes are deliberately absent: they are ordinary
/// vocabulary in source code and in protocol JSON.
const SENSITIVE_NAME_PATTERN: &str = r"api[_-]?key|access[_-]?token|auth[_-]?token|access[_-]?key|secret[_-]?key|private[_-]?key|password|passwd|secret";

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
                // No leading boundary: the credential word may end a longer
                // identifier. The separator may follow the closing quote of a
                // quoted key. A quoted value is replaced whole, through its
                // closing quote on the same line, so a passphrase with spaces
                // or punctuation does not leave a tail behind; escapes are
                // stepped over. An unterminated quote, or a quoted value that
                // contains a bracket, falls back to the bare token. Brackets
                // stay excluded so an existing marker is never matched again.
                // The name, separator and quotes are preserved byte for byte.
                Regex::new(&format!(
                    r"(?i)({SENSITIVE_NAME_PATTERN}){}(?:{}|{}|{})",
                    r#"([\"']?\s*(?::|=>?)\s*)"#,
                    r#"(")(?:[^"\\\n\[\]]|\\[^\n\[\]]){4,}(")"#,
                    r#"(')(?:[^'\\\n\[\]]|\\[^\n\[\]]){4,}(')"#,
                    r#"([\"']?)[^\s\"',;}\]\[]{4,}"#,
                ))
                .unwrap(),
                "$1$2$3$5$7[REDACTED]$4$6",
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
/// Uses the same credential words as free-text credential assignment, anchored
/// to the whole name: free text accepts any name that ends in a credential
/// word, while a source binding must be exactly one. Callers must establish the
/// AST relationship to a value; the function makes no inference about similarly
/// named functions or variables.
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
/// after [`redact_text`], under [`TEXT_POLICY_VERSION`] `redact-text@2`.
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
/// # Provenance argument for redact-text@2
///
/// Give each original UTF-8 byte six credits: JSON escaping costs at most six
/// bytes per original byte, including controls, quotes and backslashes. Each
/// private-key marker costs 22 ASCII bytes and replaces at least 52 original
/// bytes; a provider-token marker costs 16 for at least 11; an AWS marker costs
/// 25 for 20. Assignment replacement preserves captures `$1` and `$2` exactly
/// and spends at least four value characters (at least four original bytes) on
/// the ten-byte `[REDACTED]` marker. Preserved Unicode whitespace and captures
/// keep their original byte credits. The `redact-text@2` widening changes only
/// where a match may start and what `$1` and `$2` contain (a credential word
/// that ends a longer name, an optional closing key quote, `=>`); it still
/// preserves both captures and still spends at least four value bytes, so the
/// same accounting holds.
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
/// separator (`PRIVATE KEY` and `ACCESS KEY` are space-separated, which no
/// credential word allows), and brackets stop token/value matches from
/// traversing a block.
/// Newly exposed word boundaries can match remaining original bytes, but do
/// not reuse spent provenance. Thus surviving text, canonical keyword bytes
/// and inserted markers are paid for by disjoint original byte credits. No
/// assumption that redaction shrinks text, or that every replacement is inert,
/// is needed. The sixfold bound is sharp for a run of NUL characters.
pub fn direct_source_json_content_bound(utf8_bytes: usize) -> Option<usize> {
    if TEXT_POLICY_VERSION != "redact-text@2" {
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

/// Whether a rule-evidence literal is withheld as token-shaped (AC-1044):
/// [`is_token_shaped`], except a dotted lowercase message key.
pub fn rule_literal_token_shaped(value: &str) -> bool {
    is_token_shaped(value) && !is_message_key(value)
}

/// Whether a string is a dotted lowercase i18n message key such as
/// `message.cannot-transition-to-payment-when-order-is-empty` (AC-1044).
/// Rule evidence states such a key instead of withholding it as
/// token-shaped: it is product text naming a rule's outcome. The toolchain's
/// [`is_token_shaped`] policy is unchanged.
///
/// It has at least two `.`-separated segments, each made of `-`-separated
/// words, and at least four words. Every word is either lowercase ASCII
/// letters with a vowel, at most 20 of them, or at most four digits. Nothing
/// uppercase, no `_ + / =`, no word that mixes letters and digits and no long
/// run without a vowel is admitted, so base64, hex, AWS, GitHub and JWT
/// shapes, alone or after a dotted prefix, stay token-shaped.
pub fn is_message_key(value: &str) -> bool {
    let segments: Vec<&str> = value.split('.').collect();
    if segments.len() < 2 {
        return false;
    }
    let mut words = 0;
    for segment in segments {
        for word in segment.split('-') {
            let letters = (1..=20).contains(&word.len())
                && word.bytes().all(|b| b.is_ascii_lowercase())
                && word.bytes().any(|b| b"aeiouy".contains(&b));
            let number = (1..=4).contains(&word.len()) && word.bytes().all(|b| b.is_ascii_digit());
            if !letters && !number {
                return false;
            }
            words += 1;
        }
    }
    words >= 4
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-1044 (T-1044): a dotted lowercase message key is product text, not
    // a token; real token shapes stay token-shaped, alone or after a dotted
    // prefix.
    #[test]
    fn a_dotted_message_key_is_not_token_shaped_and_real_tokens_still_are() {
        for key in [
            "message.cannot-transition-to-payment-when-order-is-empty",
            "message.cannot-transition-order-contains-products-which-are-unavailable",
            "error.stockonhand-cannot-be-negative-for-this-product-variant",
            "errors.order.limit-of-100-items-reached-for-this-order",
        ] {
            assert!(key.len() >= 40, "{key}");
            assert!(is_message_key(key), "{key}");
            assert!(!rule_literal_token_shaped(key), "{key}");
        }
        for token in [
            // AWS access key id and secret access key.
            "AKIAIOSFODNN7EXAMPLE.AKIAIOSFODNN7EXAMPLEKEY",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            // GitHub token.
            "ghp_1234567890abcdefghijklmnopqrstuvwxyzAB",
            // JWT.
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N",
            // Generic high-entropy: hex, lowercase base32 and mixed words.
            "3f9a0c7be12d4e5f60718293a4b5c6d7e8f90123",
            "message.3f9a0c7be12d4e5f60718293a4b5c6d7e8f90123",
            "message.ghp_1234567890abcdefghijklmnopqrstuvwx",
            "message.cannot-transition.AKIAIOSFODNN7EXAMPLE",
            "message.qzxkvbnmlkjhgfdsrtpwc-cannot-transition",
            "message.abcdefghijklmnopqrstuvwxyzab-transition",
            "message.k8s9d7f6-cannot-transition-to-payment",
            "mnbvcxzlkjhgfdsapoiuytrewqmnbvcxzlkjhgfdsa",
        ] {
            assert!(!is_message_key(token), "{token}");
            assert!(rule_literal_token_shaped(token), "{token}");
        }
    }

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
            "accessKey",
            "SECRET_KEY",
            "private-key",
            "passwd",
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
            "dbPassword",
            "client_secret",
        ] {
            assert!(!is_sensitive_name(name));
        }
    }

    #[test]
    fn free_text_redaction_covers_compound_names_and_quoted_keys() {
        // AC-0579 / #1753 (redact-text@2): a credential word may end a longer name, and
        // the key may be quoted. Each of these was left untouched by @1.
        let cases = [
            ("DB_PASSWORD=hunter2hunter2", "DB_PASSWORD=[REDACTED]"),
            (
                "AWS_SECRET_ACCESS_KEY=abcdEFGHijklMNOPqrstUVWXyz0123456789abcd",
                "AWS_SECRET_ACCESS_KEY=[REDACTED]",
            ),
            ("client_secret: s3cr3tvalue99", "client_secret: [REDACTED]"),
            (
                "dbPassword = 'synthetic-value'",
                "dbPassword = '[REDACTED]'",
            ),
            (
                "SECRET_KEY = \"synthetic-value\"",
                "SECRET_KEY = \"[REDACTED]\"",
            ),
            ("PGPASSWORD=synthetic-value", "PGPASSWORD=[REDACTED]"),
            (
                "export MYSQL_PASSWD=synthetic",
                "export MYSQL_PASSWD=[REDACTED]",
            ),
            ("private_key: synthetic-value", "private_key: [REDACTED]"),
            (
                "{\"password\": \"hunter2hunter2\"}",
                "{\"password\": \"[REDACTED]\"}",
            ),
            ("{'api_key':'synthetic-value'}", "{'api_key':'[REDACTED]'}"),
            (
                "'db_password' => 'synthetic-value'",
                "'db_password' => '[REDACTED]'",
            ),
            // A quoted value is replaced through its closing quote: no tail
            // of a passphrase with spaces, punctuation or escapes survives.
            (
                "{\"password\": \"correct horse battery staple\"}",
                "{\"password\": \"[REDACTED]\"}",
            ),
            (
                "db_password = 'two words; and, more'",
                "db_password = '[REDACTED]'",
            ),
            (
                r#"secret: "say \"hi\" twice", next: 1"#,
                r#"secret: "[REDACTED]", next: 1"#,
            ),
            (
                "password=\"a b\\\\\" + suffix",
                "password=\"[REDACTED]\" + suffix",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_text(input), (expected.into(), 1), "{input}");
            assert_eq!(
                recognized_secrets(input),
                vec![SecretKind::CredentialAssignment],
                "{input}"
            );
            // Idempotent: a marker is never counted or rewritten again.
            assert_eq!(redact_text(expected), (expected.into(), 0), "{expected}");
            assert_direct_source_content_bound(input);
        }
    }

    #[test]
    fn free_text_redaction_still_requires_a_credential_word_at_the_end_of_the_name() {
        // AC-0579 / #1753: the widening is a suffix rule. A name that merely contains a
        // credential word, a comparison, and ordinary `token`/`key` vocabulary
        // stay as written, so recovered source keeps its evidence.
        for safe in [
            "passwordRequired: true",
            "password_hash = bcrypt(value)",
            "secrets = load_all()",
            "if (password == confirmation) return ok;",
            "next_token = tokens.advance()",
            "idempotency_key: 1234567890",
            "{\"completed_attempt_ids\":[\"ffff\"],\"refusal_count\":3}",
        ] {
            assert_eq!(redact_text(safe), (safe.into(), 0), "{safe}");
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
        assert_eq!(TEXT_POLICY_VERSION, "redact-text@2");
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

    #[test]
    fn direct_source_bound_pins_exact_redaction_policy_definition() {
        // #708: changing patterns, replacements, categories or their order requires
        // an explicit reproof/repin of the direct-source bound, not just safe examples.
        assert_eq!(TEXT_POLICY_VERSION, "redact-text@2");
        let definition = patterns()
            .iter()
            .map(|(pattern, replacement, kind)| {
                // Exhaustive on purpose: adding a SecretKind must revisit this guard.
                let category = match kind {
                    SecretKind::PrivateKey => "private_key",
                    SecretKind::BearerToken => "bearer_token",
                    SecretKind::ProviderToken => "provider_token",
                    SecretKind::AwsAccessKey => "aws_access_key",
                    SecretKind::CredentialAssignment => "credential_assignment",
                };
                (pattern.as_str(), *replacement, category)
            })
            .collect::<Vec<_>>();
        // Domain-separated BLAKE3 of compact JSON [policy, ordered triples of
        // exact regex source, replacement template and explicit category name].
        let bytes = serde_json::to_vec(&(TEXT_POLICY_VERSION, definition)).unwrap();
        let mut digest = blake3::Hasher::new();
        digest.update(b"cartograph:direct-source-json-bound-policy:v1\0");
        digest.update(&bytes);
        assert_eq!(
            digest.finalize().to_hex().as_str(),
            "622967b07389bcfbe23ac06a0e2593277747cec06c2e677d88fb05b3654df0ae"
        );
    }

    #[test]
    fn direct_source_bound_covers_deterministic_seeded_pattern_fragment_corpus() {
        // #708: supplementary exploration, not a substitute for the provenance
        // proof or exact policy pin. Fixed xorshift64 seed/steps make it reproducible.
        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        let fragments = [
            "secret",
            "password",
            "api_key",
            "ACCESS_TOKEN",
            "auth-token",
            "ſecret",
            // AC-0579 / #1753: redact-text@2 name prefixes, added words and separators.
            "DB_",
            "client",
            "access_key",
            "SECRET_KEY",
            "passwd",
            "=>",
            "=",
            ":",
            "'",
            "\"",
            "\\",
            " ",
            "\t",
            "\r\n",
            "\u{85}",
            "\u{2003}",
            "Bearer",
            "bearer abcdefgh",
            "Bearer\u{2003}abcdefgh",
            "abcdefgh",
            "aaaa",
            "sk-",
            "ghp_",
            "github_pat_",
            "AKIA",
            "0000000000000000",
            "sk-abcdefgh",
            "-----BEGIN ",
            "PRIVATE KEY-----",
            "-----END ",
            "-----BEGIN PRIVATE KEY----------END PRIVATE KEY-----",
            "secret=Bearer abcdefgh",
            "[REDACTED]",
            "[REDACTED AWS ACCESS KEY]",
            "[",
            "]",
            ",",
            ";",
            "}",
            "é",
            "漢",
            "🦀",
            "\u{2028}",
            "\u{2029}",
            "\0",
            "\u{1}",
            "\u{8}",
            "\u{b}",
            "\u{c}",
            "\u{1f}",
        ];
        let mut seed = 0x708d_1aec_5eed_2026_u64;
        let mut expanding_cases = 0;
        let mut redacting_cases = 0;
        for _ in 0..512 {
            let count = 2 + next(&mut seed) % 39;
            let mut text = String::new();
            for _ in 0..count {
                let index = (next(&mut seed) % fragments.len() as u64) as usize;
                text.push_str(fragments[index]);
            }
            let (redacted, replacements) = redact_text(&text);
            redacting_cases += usize::from(replacements > 0);
            expanding_cases += usize::from(redacted.len() > text.len());
            assert_direct_source_content_bound(&text);
        }
        // Ensure the seed still exercises replacements and actual expansion.
        assert!(redacting_cases > 100);
        assert!(expanding_cases > 0);
    }
}
