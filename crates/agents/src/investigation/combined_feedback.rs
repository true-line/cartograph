//! Schema-six text-free corrections and bounded, canonical streaming collection.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CombinedCorrection {
    ReplayAt(ReplayLocation),
    LimitationDeniesEvidence { evidence_id: String },
    EnumerationAt(ReplayLocation),
    ConfigurationAt(ReplayLocation),
    RewriteSchemaPlaceholders,
    ParaphraseProviderMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CombinedCorrections {
    pub entries: Vec<CombinedCorrection>,
    pub omitted: u64,
}

type PositionKey = (u32, u8, u32, u32);
type CorrectionKey<'a> = (u8, PositionKey, PositionKey, u8, &'a str);
fn position_key(position: &ProsePosition) -> PositionKey {
    let (finding, field, limitation) = match position.field {
        ProseField::FindingTitle { finding } => (finding, 0, 0),
        ProseField::FindingStatement { finding } => (finding, 1, 0),
        ProseField::FindingLimitation {
            finding,
            limitation,
        } => (finding, 2, limitation),
        ProseField::Limitation { limitation } => (12, 0, limitation),
        ProseField::Model => (13, 0, 0),
        ProseField::Narrative => (14, 0, 0),
    };
    (finding, field, limitation, position.sentence)
}
impl CombinedCorrection {
    fn key(&self) -> CorrectionKey<'_> {
        let zero = (0, 0, 0, 0);
        match self {
            Self::ReplayAt(location) => (
                0,
                position_key(&location.start),
                position_key(&location.end),
                0,
                "",
            ),
            Self::LimitationDeniesEvidence { evidence_id } => (1, zero, zero, 0, evidence_id),
            Self::EnumerationAt(location) => (
                2,
                position_key(&location.start),
                position_key(&location.end),
                0,
                "",
            ),
            Self::ConfigurationAt(location) => (
                2,
                position_key(&location.start),
                position_key(&location.end),
                1,
                "",
            ),
            Self::RewriteSchemaPlaceholders => (3, zero, zero, 0, ""),
            Self::ParaphraseProviderMetadata => (3, zero, zero, 1, ""),
        }
    }
    fn validate(&self, policy: &InvestigationPolicy) -> Result<(), InvestigationError> {
        match self {
            Self::ReplayAt(location)
            | Self::EnumerationAt(location)
            | Self::ConfigurationAt(location) => location.validate(),
            Self::LimitationDeniesEvidence { evidence_id } => {
                if evidence_id.strip_prefix("evidence-").is_some_and(|digits| {
                    !digits.is_empty()
                        && !digits.starts_with('0')
                        && digits.bytes().all(|b| b.is_ascii_digit())
                        && digits
                            .parse::<usize>()
                            .is_ok_and(|n| n <= policy.limits().evidence_items)
                }) {
                    Ok(())
                } else {
                    Err(InvestigationError::InvalidInput)
                }
            }
            Self::RewriteSchemaPlaceholders | Self::ParaphraseProviderMetadata => Ok(()),
        }
    }
}
impl CombinedCorrections {
    pub(super) fn validate(&self, policy: &InvestigationPolicy) -> Result<(), InvestigationError> {
        if (self.entries.is_empty() && self.omitted == 0)
            || self
                .entries
                .windows(2)
                .any(|pair| pair[0].key() >= pair[1].key())
        {
            return Err(InvestigationError::InvalidInput);
        }
        for entry in &self.entries {
            entry.validate(policy)?;
        }
        Ok(())
    }
}

impl RefusalFeedback {
    /// Consume a canonical ordered stream completely, keeping only the prefix
    /// that fits the existing whole-feedback ceiling. Adjacent duplicates count
    /// once, including after overflow. Out-of-order or malformed input fails
    /// closed even after retention fills. No source prose enters this stream.
    ///
    /// Order: replay ranges; lexical evidence IDs; claim locations (enumeration
    /// before configuration); fixed rewrite instructions. Producers must merge
    /// duplicate replay sources before streaming. The full u64 omitted marker
    /// is reserved before retaining any entry, so later count growth always fits.
    pub fn collect_combined_with_policy(
        unmet: Vec<FeedbackEntry>,
        entries: impl IntoIterator<Item = Result<CombinedCorrection, InvestigationError>>,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        Self::collect_combined_visiting_with_policy(unmet, policy, |emit| {
            for entry in entries {
                emit(entry?)?;
            }
            Ok(())
        })?
        .ok_or(InvestigationError::InvalidInput)
    }

    /// Callback-based bounded collection. Returning `None` means no correction
    /// was emitted; unmet-obligation handling remains with the finish decision.
    pub fn collect_combined_visiting_with_policy(
        unmet: Vec<FeedbackEntry>,
        policy: &InvestigationPolicy,
        visit: impl FnOnce(
            &mut dyn FnMut(CombinedCorrection) -> Result<(), InvestigationError>,
        ) -> Result<(), InvestigationError>,
    ) -> Result<Option<Self>, InvestigationError> {
        let mut feedback = Self {
            citation_drop: None,
            schema_version: 6,
            prose_correction: Some(ProseCorrection::Combined(Box::new(CombinedCorrections {
                entries: Vec::new(),
                omitted: u64::MAX,
            }))),
            citation_correction: None,
            unmet,
        };
        // Validates base obligations and proves the overflow-only envelope fits.
        feedback.bytes_with_policy(policy)?;
        let mut previous: Option<CombinedCorrection> = None;
        let mut omitted = 0u64;
        let mut full = false;
        let mut failure = None;
        let mut emit = |entry: CombinedCorrection| {
            if let Some(error) = failure {
                return Err(error);
            }
            let result = (|| {
                entry.validate(policy)?;
                if let Some(old) = &previous {
                    match old.key().cmp(&entry.key()) {
                        std::cmp::Ordering::Greater => {
                            return Err(InvestigationError::InvalidInput);
                        }
                        std::cmp::Ordering::Equal => return Ok(()),
                        std::cmp::Ordering::Less => (),
                    }
                }
                previous = Some(entry.clone());
                if !full {
                    let Some(ProseCorrection::Combined(combined)) = &mut feedback.prose_correction
                    else {
                        unreachable!()
                    };
                    combined.entries.push(entry);
                    if bounded_bytes(&feedback, feedback_bytes(policy)?).is_err() {
                        let Some(ProseCorrection::Combined(combined)) =
                            &mut feedback.prose_correction
                        else {
                            unreachable!()
                        };
                        combined.entries.pop();
                        full = true;
                    }
                }
                if full {
                    omitted = omitted
                        .checked_add(1)
                        .ok_or(InvestigationError::LimitExceeded)?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                failure = Some(error);
            }
            result
        };
        visit(&mut emit)?;
        if let Some(error) = failure {
            return Err(error);
        }
        if previous.is_none() {
            return Ok(None);
        }
        let Some(ProseCorrection::Combined(combined)) = &mut feedback.prose_correction else {
            unreachable!()
        };
        combined.omitted = omitted;
        feedback.bytes_with_policy(policy)?;
        Ok(Some(feedback))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn replay(sentence: u32) -> CombinedCorrection {
        let position = ProsePosition {
            field: ProseField::Narrative,
            sentence,
        };
        CombinedCorrection::ReplayAt(ReplayLocation {
            start: position.clone(),
            end: position,
        })
    }
    #[test]
    fn combined_feedback_callback_errors_cannot_be_ignored() {
        let result = RefusalFeedback::collect_combined_visiting_with_policy(
            vec![],
            &INVESTIGATION_V1_POLICY,
            |emit| {
                emit(replay(2))?;
                let _ = emit(replay(1));
                let _ = emit(replay(3));
                Ok(())
            },
        );
        assert_eq!(result, Err(InvestigationError::InvalidInput));
        assert_eq!(
            RefusalFeedback::collect_combined_visiting_with_policy(
                vec![],
                &INVESTIGATION_V1_POLICY,
                |_| Ok(())
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn combined_feedback_counts_every_unique_omission_within_existing_cap() {
        let policy = &INVESTIGATION_V1_POLICY;
        let feedback = RefusalFeedback::collect_combined_with_policy(
            vec![],
            (1..=200).flat_map(|n| [Ok(replay(n)), Ok(replay(n))]),
            policy,
        )
        .unwrap();
        let Some(ProseCorrection::Combined(combined)) = &feedback.prose_correction else {
            panic!()
        };
        assert!(!combined.entries.is_empty());
        assert!(combined.omitted > 0);
        assert_eq!(combined.entries.len() as u64 + combined.omitted, 200);
        assert!(
            feedback.bytes().unwrap().len()
                <= RefusalFeedback::max_bytes_with_policy(policy).unwrap()
        );
        let wire = feedback.bytes().unwrap();
        let decoded: RefusalFeedback = serde_json::from_slice(&wire).unwrap();
        assert_eq!(decoded.bytes().unwrap(), wire);
        let mut unknown = decoded.clone();
        unknown.schema_version = 7;
        assert!(unknown.bytes().is_err());
        let mut historical_version = decoded;
        historical_version.schema_version = 5;
        assert!(historical_version.bytes().is_err());
    }
    #[test]
    fn combined_feedback_keeps_both_claim_classes_and_evidence_ids() {
        let CombinedCorrection::ReplayAt(location) = replay(1) else {
            panic!()
        };
        let entries = vec![
            replay(1),
            CombinedCorrection::LimitationDeniesEvidence {
                evidence_id: "evidence-1".into(),
            },
            CombinedCorrection::EnumerationAt(location.clone()),
            CombinedCorrection::ConfigurationAt(location),
            CombinedCorrection::RewriteSchemaPlaceholders,
        ];
        let feedback = RefusalFeedback::collect_combined_with_policy(
            vec![],
            entries.clone().into_iter().map(Ok),
            &INVESTIGATION_V1_POLICY,
        )
        .unwrap();
        let wire = feedback.bytes().unwrap();
        let text = String::from_utf8(wire).unwrap();
        assert!(!text.contains('\\'));
        assert_eq!(core_redact::redact_text(&text).1, 0);
        assert!(serde_json::from_str::<CombinedCorrection>(r#"{"limitation_denies_evidence":{"evidence_id":"evidence-1","extra":"not structural"}}"#).is_err());
        let Some(ProseCorrection::Combined(combined)) = feedback.prose_correction else {
            panic!()
        };
        assert_eq!(combined.entries, entries);
        assert_eq!(combined.omitted, 0);
    }
    #[test]
    fn combined_feedback_validates_after_overflow_and_rejects_noncanonical_streams() {
        let policy = &INVESTIGATION_V1_POLICY;
        assert!(
            RefusalFeedback::collect_combined_with_policy(
                vec![],
                (1..=200).map(|n| Ok(replay(n))).chain([Ok(replay(1))]),
                policy
            )
            .is_err()
        );
        assert!(
            RefusalFeedback::collect_combined_with_policy(
                vec![],
                (1..=200)
                    .map(|n| Ok(replay(n)))
                    .chain([Err(InvestigationError::InvalidInput)]),
                policy
            )
            .is_err()
        );
        assert!(
            RefusalFeedback::collect_combined_with_policy(
                vec![],
                [Ok(CombinedCorrection::LimitationDeniesEvidence {
                    evidence_id: "evidence-01".into()
                })],
                policy
            )
            .is_err()
        );
        assert!(RefusalFeedback::collect_combined_with_policy(vec![], [], policy).is_err());
    }
}

#[path = "combined_feedback_input.rs"]
mod input_collection;
