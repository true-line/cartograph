//! Text-free positions in rejected authored prose; never an admission authority.
use super::{InvestigationError, MAX_INPUT_BYTES, privacy};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "field", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProseField {
    FindingTitle { finding: u32 },
    FindingStatement { finding: u32 },
    FindingLimitation { finding: u32, limitation: u32 },
    Limitation { limitation: u32 },
    Model,
    Narrative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProsePosition {
    pub field: ProseField,
    /// One-based; a punctuation run followed by whitespace ends a sentence.
    pub sentence: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayLocation {
    pub start: ProsePosition,
    pub end: ProsePosition,
}

impl ReplayLocation {
    /// Shape/order validation only. It cannot establish that a detector match
    /// occurred; saved feedback also needs the host's invocation witness.
    pub fn validate(&self) -> Result<(), InvestigationError> {
        let key = |position: &ProsePosition| {
            if position.sentence == 0 || position.sentence as usize > MAX_INPUT_BYTES {
                return None;
            }
            // These are the existing admitted Finish shape bounds. Field order
            // must match validate_correction_prose, including within findings.
            let field = match position.field {
                ProseField::FindingTitle { finding } if finding < 12 => (finding, 0, 0),
                ProseField::FindingStatement { finding } if finding < 12 => (finding, 1, 0),
                ProseField::FindingLimitation {
                    finding,
                    limitation,
                } if finding < 12 && limitation < 8 => (finding, 2, limitation),
                ProseField::Limitation { limitation } if limitation < 8 => (12, 0, limitation),
                ProseField::Model => (13, 0, 0),
                ProseField::Narrative => (14, 0, 0),
                _ => return None,
            };
            Some((field, position.sentence))
        };
        match (key(&self.start), key(&self.end)) {
            (Some(start), Some(end)) if start <= end => Ok(()),
            _ => Err(InvestigationError::InvalidInput),
        }
    }
}

/// Logical additional owners of `locate_prose_replay`, including its detector
/// preflight. Borrowed fields/supplied strings and native redaction machinery
/// are excluded. Bounds are existing detector admission caps, not new policy.
/// The host must charge this before activating diagnostic feedback.
pub fn replay_location_workspace_bytes() -> Option<usize> {
    use context_hub::logical::{map, set, slots, sum};
    let redacted = core_redact::direct_source_json_content_bound(MAX_INPUT_BYTES)?;
    let source = sum(&[
        redacted.checked_mul(2)?, // consecutive redaction outputs overlap
        slots::<String>(2)?,
        slots::<char>(redacted.max(MAX_INPUT_BYTES))?,
        slots::<Vec<char>>(1)?,
    ])?;
    let detector = sum(&[
        MAX_INPUT_BYTES, // joined prose
        slots::<String>(1)?,
        slots::<char>(MAX_INPUT_BYTES)?,
        slots::<Vec<char>>(1)?,
        set::<&[char]>(MAX_INPUT_BYTES)?,
        slots::<std::collections::HashSet<&[char]>>(1)?,
        source,
    ])?;
    let diagnostic = sum(&[
        slots::<char>(MAX_INPUT_BYTES)?,
        slots::<Vec<char>>(1)?,
        slots::<(usize, u32)>(MAX_INPUT_BYTES)?,
        slots::<Vec<(usize, u32)>>(1)?,
        map::<&[char], usize>(MAX_INPUT_BYTES)?,
        slots::<HashMap<&[char], usize>>(1)?,
        slots::<ReplayLocation>(1)?,
        source,
    ])?;
    // The detector returns and releases its owners before diagnostics start.
    Some(detector.max(diagnostic))
}

/// Diagnose the first detector match in supplied-item order, returning only
/// field/sentence positions. All decisions remain with the unchanged detector.
/// The caller must bind these positions to the refused finish and invocation.
pub fn locate_prose_replay(
    fields: &[(ProseField, &str)],
    supplied: &[&str],
    metadata: bool,
) -> Result<Option<ReplayLocation>, InvestigationError> {
    // Bound even empty fields before allocating position/index vectors.
    if fields.len() > MAX_INPUT_BYTES || supplied.len() > 16384 {
        return Err(InvestigationError::LimitExceeded);
    }
    let prose = fields.iter().map(|(_, text)| *text);
    let verdict = if metadata {
        privacy::reject_metadata_replay(prose, supplied.iter().copied())
    } else {
        privacy::reject_prose_replay(prose, supplied.iter().copied())
    };
    match verdict {
        Ok(()) => return Ok(None),
        Err(InvestigationError::SourceReplay) => (),
        Err(error) => return Err(error),
    }
    let mut output = Vec::new();
    let mut positions = Vec::new();
    for (field_index, (_, text)) in fields.iter().enumerate() {
        let mut sentence = 1u32;
        let mut terminal = false;
        for character in text.chars() {
            if character.is_whitespace() {
                if terminal {
                    sentence = sentence
                        .checked_add(1)
                        .ok_or(InvestigationError::LimitExceeded)?;
                    terminal = false;
                }
            } else {
                output.push(character);
                positions.push((field_index, sentence));
                terminal = matches!(character, '.' | '!' | '?' | '。' | '！' | '？');
            }
        }
    }
    let mut windows = HashMap::new();
    for (index, window) in output.windows(48).enumerate() {
        windows.entry(window).or_insert(index);
    }
    let position = |offset: usize| {
        let (field, sentence) = positions[offset];
        ProsePosition {
            field: fields[field].0.clone(),
            sentence,
        }
    };
    let locate = |text: &str| {
        let source = text
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<Vec<_>>();
        let found = if source.is_empty() {
            None
        } else if source.len() < 48 {
            if metadata {
                None
            } else {
                output
                    .windows(source.len())
                    .position(|window| window == source)
                    .map(|start| (start, source.len()))
            }
        } else {
            source
                .windows(48)
                .find_map(|window| windows.get(window).map(|start| (*start, 48)))
        };
        found.map(|(start, length)| ReplayLocation {
            start: position(start),
            end: position(start + length - 1),
        })
    };
    let mut bytes = 0usize;
    for item in supplied {
        bytes = bytes
            .checked_add(item.len())
            .ok_or(InvestigationError::LimitExceeded)?;
        if bytes > MAX_INPUT_BYTES {
            return Err(InvestigationError::LimitExceeded);
        }
        if let Some(location) = locate(item) {
            return Ok(Some(location));
        }
        let (redacted, replacements) = core_redact::redact_text(item);
        if replacements > 0
            && let Some(location) = locate(&redacted)
        {
            return Ok(Some(location));
        }
    }
    // Never allow a diagnostic discrepancy to turn a refusal into admission.
    Err(InvestigationError::SourceReplay)
}

#[cfg(test)]
mod tests {
    // AC-0287 / Decision #1692: diagnostics do not relax admission.
    use super::*;
    #[test]
    fn replay_location_rejects_reversed_and_out_of_shape_positions() {
        let mut location = ReplayLocation {
            start: ProsePosition {
                field: ProseField::FindingStatement { finding: 0 },
                sentence: 2,
            },
            end: ProsePosition {
                field: ProseField::FindingTitle { finding: 1 },
                sentence: 1,
            },
        };
        assert_eq!(location.validate(), Ok(()));
        std::mem::swap(&mut location.start, &mut location.end);
        assert_eq!(location.validate(), Err(InvestigationError::InvalidInput));
        location.start = location.end.clone();
        location.start.sentence = 0;
        assert_eq!(location.validate(), Err(InvestigationError::InvalidInput));
        location.start = location.end.clone();
        location.end.field = ProseField::FindingTitle { finding: 12 };
        assert_eq!(location.validate(), Err(InvestigationError::InvalidInput));
        location.end.field = ProseField::FindingLimitation {
            finding: 0,
            limitation: 8,
        };
        assert_eq!(location.validate(), Err(InvestigationError::InvalidInput));
        location.end = location.start.clone();
        assert_eq!(location.validate(), Ok(()));
        location.end.sentence = 1;
        assert_eq!(location.validate(), Err(InvestigationError::InvalidInput));
    }

    #[test]
    fn replay_location_reports_unicode_and_sentence_ranges() {
        let source = "界".repeat(48);
        let prose = format!(
            "Earlier?!\n{}！\u{2003}{}",
            "界".repeat(24),
            "界".repeat(24)
        );
        // The punctuation is itself part of the normalized detector window.
        let source = format!("{}！{}", &source[..24 * 3], &source[24 * 3..]);
        let location = locate_prose_replay(&[(ProseField::Narrative, &prose)], &[&source], false)
            .unwrap()
            .unwrap();
        assert_eq!(location.start.sentence, 2);
        assert_eq!(location.end.sentence, 3);
        let oversized = "x".repeat(MAX_INPUT_BYTES + 1);
        assert_eq!(
            locate_prose_replay(&[(ProseField::Narrative, &oversized)], &["x"], false),
            Err(InvestigationError::LimitExceeded)
        );
    }

    #[test]
    fn replay_location_spans_fields_without_echoing_text() {
        let left = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let right = "abcdefghijklmnopqrstuvwxyz";
        let source = format!("{left}{right}");
        let fields = [
            (ProseField::FindingStatement { finding: 0 }, left),
            (ProseField::Narrative, right),
        ];
        let location = locate_prose_replay(&fields, &[&source], false)
            .unwrap()
            .unwrap();
        assert_eq!(location.start.field, fields[0].0);
        assert_eq!(location.end.field, ProseField::Narrative);
        assert_eq!(location.start.sentence, 1);
        let wire = serde_json::to_string(&location).unwrap();
        assert!(!wire.contains(left) && !wire.contains(right));
        assert_eq!(
            privacy::reject_prose_replay([left, right], [source.as_str()]),
            Err(InvestigationError::SourceReplay)
        );
    }
    #[test]
    fn replay_location_tracks_sentence_and_preserves_metadata_and_sensitive_verdicts() {
        let fields = [(
            ProseField::Narrative,
            "Independent sentence. ExactSymbol is referenced.",
        )];
        let location = locate_prose_replay(&fields, &["ExactSymbol"], false)
            .unwrap()
            .unwrap();
        assert_eq!(location.start.sentence, 2);
        assert_eq!(location.end.sentence, 2);
        assert!(
            locate_prose_replay(&fields, &["ExactSymbol"], true)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            locate_prose_replay(
                &[(ProseField::Narrative, "password=synthetic-secret")],
                &[],
                false
            ),
            Err(InvestigationError::SensitiveOutput)
        );
    }
}

#[path = "replay_locations.rs"]
mod complete;
pub use complete::{
    replay_locations_workspace_bytes, visit_prose_replays, visit_prose_replays_with_metadata,
};
