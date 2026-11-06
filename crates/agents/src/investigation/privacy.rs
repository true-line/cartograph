use super::{InvestigationError, MAX_INPUT_BYTES, text};
use std::collections::HashSet;

/// A bounded redacted copy for durable question/history fields. The original
/// request fingerprint is computed independently by the host for deduplication.
pub fn redacted_history_text(input: &str, maximum: usize) -> Result<String, InvestigationError> {
    if !text(input, maximum) || maximum > MAX_INPUT_BYTES {
        return Err(InvestigationError::InvalidRequest);
    }
    let (redacted, _) = core_redact::redact_text(input);
    if !text(&redacted, maximum) {
        return Err(InvestigationError::LimitExceeded);
    }
    Ok(redacted)
}

/// The existing 48 non-whitespace scalar replay threshold, including complete
/// short items, applied to every supplied string and joined model-authored prose.
/// Typed citation IDs are separately membership-checked, never passed as prose.
/// This is bounded exact-copy rejection, not semantic declassification.
pub fn reject_prose_replay<'p, 's>(
    prose: impl IntoIterator<Item = &'p str>,
    supplied: impl IntoIterator<Item = &'s str>,
) -> Result<(), InvestigationError> {
    reject_replay(prose, supplied, true)
}

/// Short graph metadata values are valid vocabulary (a symbol can be named a).
/// Long values/keys still receive the exact-copy window; raw source excerpts use
/// the stronger complete-short-item rule in reject_prose_replay above.
pub(super) fn reject_metadata_replay<'p, 's>(
    prose: impl IntoIterator<Item = &'p str>,
    supplied: impl IntoIterator<Item = &'s str>,
) -> Result<(), InvestigationError> {
    reject_replay(prose, supplied, false)
}

fn reject_replay<'p, 's>(
    prose: impl IntoIterator<Item = &'p str>,
    supplied: impl IntoIterator<Item = &'s str>,
    reject_short: bool,
) -> Result<(), InvestigationError> {
    const WINDOW: usize = 48;
    let normalize = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<Vec<_>>();
    let mut joined = String::new();
    for part in prose {
        if part.len() > MAX_INPUT_BYTES.saturating_sub(joined.len()) {
            return Err(InvestigationError::LimitExceeded);
        }
        if core_redact::redact_text(part).1 != 0 {
            return Err(InvestigationError::SensitiveOutput);
        }
        joined.push_str(part);
    }
    let output = normalize(&joined);
    let excerpts: HashSet<&[char]> = output.windows(WINDOW).collect();
    let mut bytes = 0usize;
    let mut items = 0usize;
    for item in supplied {
        items += 1;
        bytes = bytes
            .checked_add(item.len())
            .ok_or(InvestigationError::LimitExceeded)?;
        if bytes > MAX_INPUT_BYTES || items > 16384 {
            return Err(InvestigationError::LimitExceeded);
        }
        let repeats = |source: Vec<char>| {
            if source.is_empty() {
                false
            } else if source.len() < WINDOW {
                reject_short && output.windows(source.len()).any(|part| part == source)
            } else {
                source.windows(WINDOW).any(|part| excerpts.contains(part))
            }
        };
        if repeats(normalize(item)) {
            return Err(InvestigationError::SourceReplay);
        }
        let (redacted, replacements) = core_redact::redact_text(item);
        if replacements > 0 && repeats(normalize(&redacted)) {
            return Err(InvestigationError::SourceReplay);
        }
    }
    Ok(())
}

#[cfg(test)]
mod detector_audit {
    use super::*;

    #[test]
    fn identifier_vocabulary_depends_on_source_vs_metadata_and_exact_window() {
        let short = "InventoryStrategy";
        let prose = "InventoryStrategy chooses a location.";
        assert_eq!(
            reject_prose_replay([prose], [short]),
            Err(InvestigationError::SourceReplay)
        );
        assert!(reject_metadata_replay([prose], [short]).is_ok());
        for width in [47, 48, 49] {
            let identifier = "X".repeat(width);
            let prose = format!("The {identifier} symbol is referenced.");
            assert_eq!(
                reject_prose_replay([prose.as_str()], [identifier.as_str()]),
                Err(InvestigationError::SourceReplay)
            );
            assert_eq!(
                reject_metadata_replay([prose.as_str()], [identifier.as_str()]).is_err(),
                width >= 48
            );
        }
    }

    #[test]
    fn concatenation_can_create_a_window_absent_from_each_authored_field() {
        let left = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let right = "abcdefghijklmnopqrstuvwxyz";
        let source = format!("prefix {left}{right} suffix");
        assert!(reject_prose_replay([left], [source.as_str()]).is_ok());
        assert!(reject_prose_replay([right], [source.as_str()]).is_ok());
        assert_eq!(
            reject_prose_replay([left, right], [source.as_str()]),
            Err(InvestigationError::SourceReplay)
        );
        // A whitespace separator is ineffective: normalization removes it.
        assert_eq!(
            reject_prose_replay([left, " ", right], [source.as_str()]),
            Err(InvestigationError::SourceReplay)
        );
    }

    #[test]
    fn within_field_replay_survives_whitespace_and_unicode_scalars() {
        let source = "界".repeat(48);
        let spaced = source
            .chars()
            .map(|c| format!("{c}\u{2003}"))
            .collect::<String>();
        assert_eq!(
            reject_prose_replay([spaced.as_str()], [source.as_str()]),
            Err(InvestigationError::SourceReplay)
        );
        assert!(reject_prose_replay(["An independent explanation."], [source.as_str()]).is_ok());
        assert!(reject_prose_replay([""], [""]).is_ok());
    }
}
