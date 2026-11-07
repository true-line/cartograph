//! AC-0287: terminal diagnostics identify the refusal without retaining prose.
use super::exhausted_finish_message;
use agents::investigation::staged_finish::{
    CombinedCorrection, CombinedCorrections, ProseCorrection,
};

#[test]
fn combined_refusals_do_not_report_placeholder_text() {
    // Mixed/omitted feedback must not be mislabeled as any single failure class.
    let correction = ProseCorrection::Combined(Box::new(CombinedCorrections {
        entries: vec![CombinedCorrection::RewriteSchemaPlaceholders],
        omitted: 1,
    }));
    assert_eq!(
        exhausted_finish_message(Some(&correction)),
        "The answer still failed verification after one correction. No findings were saved."
    );
}

#[test]
fn placeholder_and_replay_diagnostics_remain_distinct() {
    assert!(
        exhausted_finish_message(Some(
            &ProseCorrection::RewriteSchemaPlaceholdersAsPlainProse
        ))
        .contains("placeholder text")
    );
    assert!(
        exhausted_finish_message(Some(&ProseCorrection::ParaphraseWithoutQuotingSource))
            .contains("source-replay")
    );
    assert_eq!(
        exhausted_finish_message(None),
        "Two well-formed finishes left completion obligations unmet."
    );
}

#[test]
fn claim_refusals_do_not_report_placeholder_text() {
    use agents::investigation::{ProseField, ProsePosition, ReplayLocation};
    let position = ProsePosition {
        field: ProseField::FindingStatement { finding: 0 },
        sentence: 1,
    };
    let location = ReplayLocation {
        start: position.clone(),
        end: position,
    };
    for correction in [
        ProseCorrection::UnverifiedEnumerationClaimAt(location.clone()),
        ProseCorrection::UnverifiedConfigurationClaimAt(location),
    ] {
        assert_eq!(
            exhausted_finish_message(Some(&correction)),
            "The answer still contained unverified claims after one correction. No findings were saved."
        );
    }
}
