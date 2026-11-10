//! Measurements only: callers must authenticate the captured range and policy.
use std::io::{self, Write};

/// Exact direct JSON string content sizes, excluding the enclosing quotes.
/// These numbers grant no source access or admission authority. Receipt users
/// must bind them to the object, range and [`crate::TEXT_POLICY_VERSION`], and
/// remeasure after the charged verified read before installing source context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectSourceJsonContentSizes {
    /// Serialized content bytes before redaction.
    pub original: usize,
    /// Serialized content bytes after all production redaction passes.
    pub redacted: usize,
}

/// Measure the actual serializer and sequential production redactor. This reads
/// the supplied text: use only where source access is already authorized.
/// No serialized source copy is retained. Overflow/serialization failure is not
/// a zero-length measurement and must not select a smaller reservation.
pub fn measure_direct_source_json_content(input: &str) -> Option<DirectSourceJsonContentSizes> {
    if crate::TEXT_POLICY_VERSION != "redact-text@2" {
        return None;
    }
    let original = content_size(input)?;
    let (redacted, _) = crate::redact_text(input);
    Some(DirectSourceJsonContentSizes {
        original,
        redacted: content_size(&redacted)?,
    })
}

fn content_size(input: &str) -> Option<usize> {
    #[derive(Default)]
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| io::Error::other("source size overflow"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter::default();
    serde_json::to_writer(&mut counter, input).ok()?;
    counter.0.checked_sub(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-0287 / #1701: exact sizing must match wire bytes, not UTF-8 length.
    #[test]
    fn matches_serializer_for_controls_unicode_and_quotes() {
        let controls: String = (0..=31).map(char::from).collect();
        for input in ["", "plain", "\"\\\n\t", "é🦀", &controls] {
            let sizes = measure_direct_source_json_content(input).unwrap();
            let (redacted, _) = crate::redact_text(input);
            assert_eq!(sizes.original, serde_json::to_vec(input).unwrap().len() - 2);
            assert_eq!(
                sizes.redacted,
                serde_json::to_vec(&redacted).unwrap().len() - 2
            );
            let fallback = crate::direct_source_json_content_bound(input.len()).unwrap();
            assert!(sizes.original <= fallback && sizes.redacted <= fallback);
        }
    }

    // AC-0287 / #1701: the reservation must cover redaction growth too.
    #[test]
    fn redaction_expansion_and_sequential_patterns_are_measured() {
        let sizes = measure_direct_source_json_content("secret=abcd").unwrap();
        assert_eq!(
            sizes,
            DirectSourceJsonContentSizes {
                original: 11,
                redacted: 17
            }
        );
        let input = "secret=Bearer abcdefgh";
        let (redacted, _) = crate::redact_text(input);
        assert_eq!(redacted, "secret=[REDACTED] [REDACTED]");
        let sizes = measure_direct_source_json_content(input).unwrap();
        assert_eq!(sizes.redacted, redacted.len());
        assert!(sizes.redacted > sizes.original);
    }
}
