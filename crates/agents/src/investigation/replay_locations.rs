//! Complete replay diagnostics. Historical first-match locator remains unchanged.
use super::*;
use std::collections::BTreeSet;

/// Additional bounded workspace for complete replay diagnostics. A bit mask per
/// output scalar records matched lengths 1..=48. For each starting sentence,
/// endpoints are either that sentence or within the following 47 scalars.
/// Callers must charge this workspace before invoking the visitor.
pub fn replay_locations_workspace_bytes() -> Option<usize> {
    use context_hub::logical::{set, slots, sum};
    sum(&[
        replay_location_workspace_bytes()?,
        slots::<u64>(MAX_INPUT_BYTES)?,
        slots::<Vec<u64>>(1)?,
        set::<(usize, u32)>(48)?,
        slots::<BTreeSet<(usize, u32)>>(1)?,
    ])
}

/// Visit every distinct matched field/sentence range in authored order. Short
/// source items retain the complete-item rule; short metadata remains allowed.
/// The callback cannot stop the scan when its retained feedback list is full.
/// This diagnoses rejection only and grants no admission authority.
pub fn visit_prose_replays(
    fields: &[(ProseField, &str)],
    supplied: &[&str],
    metadata: bool,
    visit: impl FnMut(ReplayLocation),
) -> Result<(), InvestigationError> {
    visit_groups(fields, &[(supplied, metadata)], visit)
}

/// Collect source and graph-metadata matches together, deduplicating shared
/// locations while preserving each group's existing short-item policy.
pub fn visit_prose_replays_with_metadata(
    fields: &[(ProseField, &str)],
    sources: &[&str],
    metadata: &[&str],
    visit: impl FnMut(ReplayLocation),
) -> Result<(), InvestigationError> {
    visit_groups(fields, &[(sources, false), (metadata, true)], visit)
}

fn visit_groups(
    fields: &[(ProseField, &str)],
    groups: &[(&[&str], bool)],
    mut visit: impl FnMut(ReplayLocation),
) -> Result<(), InvestigationError> {
    if fields.len() > MAX_INPUT_BYTES {
        return Err(InvestigationError::LimitExceeded);
    }
    // Authored field order is canonical and each field occurs once. Enforce
    // this before callbacks so position grouping also establishes deduplication.
    let mut previous: Option<ProsePosition> = None;
    for (field, _) in fields {
        let position = ProsePosition {
            field: field.clone(),
            sentence: 1,
        };
        if previous
            .as_ref()
            .is_some_and(|old| old.field == position.field)
        {
            return Err(InvestigationError::InvalidInput);
        }
        ReplayLocation {
            start: previous.clone().unwrap_or_else(|| position.clone()),
            end: position.clone(),
        }
        .validate()?;
        previous = Some(position);
    }
    // The legacy detector may stop at its first match. Validate the entire
    // supplied sequence before allocating or publishing complete diagnostics.
    let mut refused = false;
    for (supplied, metadata) in groups {
        let bytes = supplied
            .iter()
            .try_fold(0usize, |total, item| total.checked_add(item.len()))
            .ok_or(InvestigationError::LimitExceeded)?;
        if supplied.len() > 16384 || bytes > MAX_INPUT_BYTES {
            return Err(InvestigationError::LimitExceeded);
        }
        let prose = fields.iter().map(|(_, text)| *text);
        let verdict = if *metadata {
            privacy::reject_metadata_replay(prose, supplied.iter().copied())
        } else {
            privacy::reject_prose_replay(prose, supplied.iter().copied())
        };
        match verdict {
            Ok(()) => (),
            Err(InvestigationError::SourceReplay) => refused = true,
            Err(error) => return Err(error),
        }
    }
    if !refused {
        return Ok(());
    }
    let mut output = Vec::new();
    let mut positions = Vec::new();
    for (field_index, (field, text)) in fields.iter().enumerate() {
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
                let position = ProsePosition {
                    field: field.clone(),
                    sentence,
                };
                ReplayLocation {
                    start: position.clone(),
                    end: position,
                }
                .validate()?;
                output.push(character);
                positions.push((field_index, sentence));
                terminal = matches!(character, '.' | '!' | '?' | '。' | '！' | '？');
            }
        }
    }
    // Only one boolean per distinct long window: repeated source windows never
    // trigger a walk over repeated output occurrences.
    let mut windows: HashMap<&[char], bool> =
        output.windows(48).map(|window| (window, false)).collect();
    let mut matches = vec![0u64; output.len()];
    let mut mark = |text: &str, metadata: bool| {
        let source: Vec<_> = text.chars().filter(|c| !c.is_whitespace()).collect();
        match source.len() {
            0 => (),
            length @ 1..=47 if !metadata => {
                for (start, window) in output.windows(length).enumerate() {
                    if window == source {
                        matches[start] |= 1u64 << (length - 1);
                    }
                }
            }
            1..=47 => (),
            _ => {
                for window in source.windows(48) {
                    if let Some(found) = windows.get_mut(window) {
                        *found = true;
                    }
                }
            }
        }
    };
    for (supplied, metadata) in groups {
        for item in *supplied {
            mark(item, *metadata);
            let (redacted, replacements) = core_redact::redact_text(item);
            if replacements > 0 {
                mark(&redacted, *metadata);
            }
        }
    }
    for (start, window) in output.windows(48).enumerate() {
        if windows.get(window) == Some(&true) {
            matches[start] |= 1u64 << 47;
        }
    }
    let position = |(field, sentence): (usize, u32)| ProsePosition {
        field: fields[field].0.clone(),
        sentence,
    };
    let mut cursor = 0;
    let mut emitted = false;
    while cursor < positions.len() {
        let start = positions[cursor];
        let mut ends = BTreeSet::new();
        while cursor < positions.len() && positions[cursor] == start {
            let mut lengths = matches[cursor];
            while lengths != 0 {
                let offset = lengths.trailing_zeros() as usize;
                ends.insert(positions[cursor + offset]);
                lengths &= lengths - 1;
            }
            cursor += 1;
        }
        for end in ends {
            let location = ReplayLocation {
                start: position(start),
                end: position(end),
            };
            location.validate()?;
            visit(location);
            emitted = true;
        }
    }
    if emitted {
        Ok(())
    } else {
        Err(InvestigationError::SourceReplay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // AC-0287 / #1701: every occurrence, deduplication and no early truncation.
    #[test]
    fn complete_replay_locations_deduplicate_and_continue_after_retention_fills() {
        let fields = [(
            ProseField::Narrative,
            "ExactSymbol. ExactSymbol. ExactSymbol.",
        )];
        let mut kept = Vec::new();
        let mut omitted = 0;
        visit_prose_replays(
            &fields,
            &["ExactSymbol", "ExactSymbol"],
            false,
            |location| {
                if kept.is_empty() {
                    kept.push(location);
                } else {
                    omitted += 1;
                }
            },
        )
        .unwrap();
        assert_eq!(kept[0].start.sentence, 1);
        assert_eq!(omitted, 2);
        let mut count = 0;
        visit_prose_replays(&fields, &["ExactSymbol"], true, |_| count += 1).unwrap();
        assert_eq!(count, 0);
        assert!(
            replay_locations_workspace_bytes().unwrap()
                > replay_location_workspace_bytes().unwrap()
        );
    }

    #[test]
    fn complete_replay_locations_fail_before_callbacks_for_invalid_inputs() {
        let mut callbacks = 0;
        let mut visit = |_| callbacks += 1;
        let repeated = [
            (ProseField::Narrative, "ExactSymbol"),
            (ProseField::Narrative, "ExactSymbol"),
        ];
        assert_eq!(
            visit_prose_replays(&repeated, &["ExactSymbol"], false, &mut visit),
            Err(InvestigationError::InvalidInput)
        );
        let oversized = "x".repeat(MAX_INPUT_BYTES);
        let fields = [(ProseField::Narrative, "ExactSymbol")];
        assert_eq!(
            visit_prose_replays(&fields, &["ExactSymbol", &oversized], false, &mut visit),
            Err(InvestigationError::LimitExceeded)
        );
        assert_eq!(
            visit_prose_replays(
                &[(ProseField::Narrative, "password=synthetic-secret")],
                &[],
                false,
                &mut visit
            ),
            Err(InvestigationError::SensitiveOutput)
        );
        assert_eq!(callbacks, 0);
    }

    #[test]
    fn complete_replay_locations_union_metadata_without_duplicate_counts() {
        let long = "界".repeat(48);
        let prose = format!("ExactSymbol. {long}.");
        let fields = [(ProseField::Narrative, prose.as_str())];
        let mut seen = Vec::new();
        visit_prose_replays_with_metadata(
            &fields,
            &["ExactSymbol", &long],
            &["ExactSymbol", &long],
            |location| seen.push(location),
        )
        .unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].start.sentence, 1);
        assert_eq!(seen[1].start.sentence, 2);
        let mut metadata_only = Vec::new();
        visit_prose_replays_with_metadata(&fields, &[], &["ExactSymbol", &long], |location| {
            metadata_only.push(location)
        })
        .unwrap();
        assert_eq!(metadata_only, seen[1..]);
        let mut mixed = Vec::new();
        visit_prose_replays_with_metadata(&fields, &["ExactSymbol"], &[&long], |location| {
            mixed.push(location)
        })
        .unwrap();
        assert_eq!(mixed, seen);
    }

    #[test]
    fn complete_replay_locations_find_all_long_windows_and_cross_field_matches() {
        let source = "界".repeat(48);
        let prose = format!("{source}. {source}.");
        let fields = [
            (ProseField::FindingTitle { finding: 0 }, "界"),
            (ProseField::Narrative, prose.as_str()),
        ];
        let mut seen = Vec::new();
        visit_prose_replays(&fields, &[&source], false, |location| seen.push(location)).unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].start.field, fields[0].0);
        assert_eq!(seen[0].end.field, ProseField::Narrative);
        assert_eq!(seen[2].start.sentence, 2);
        assert!(!serde_json::to_string(&seen).unwrap().contains('界'));
    }
}
