//! Text-free refusal diagnostics retained separately from admission accounting.
use super::{InvestigationError, ReplayLocation, bounded_bytes, claim_diagnostics::*};
use serde::{Deserialize, Serialize};

/// Owner-approved serialized sidecar allowance (#1707), independent of store limits.
pub const MAX_REFUSAL_DIAGNOSTIC_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub location: ReplayLocation,
    /// Index into the authenticated finish finding's citation list, not source text.
    pub citation_index: Option<usize>,
    pub detail: Detail,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Detail {
    Enumeration {
        diagnostic: EnumerationDiagnostic,
    },
    Configuration {
        predicate_token: usize,
        reason: ConfigurationFailure,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Retention {
    Recorded {
        entries: Vec<Entry>,
        omitted_entries: usize,
    },
    Unrecorded {
        reason: UnrecordedReason,
        omitted_entries: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnrecordedReason {
    Capacity,
    /// Collection failed before a trustworthy omission count was available.
    CollectorRejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sidecar {
    pub schema_version: u32,
    pub refusal_hash: String,
    pub finish_hash: String,
    pub retention: Retention,
}

impl Sidecar {
    /// Retain a deterministic prefix. The iterator is exhausted even after the
    /// budget fills so omissions describe the complete detector traversal.
    pub fn collect(
        refusal_hash: String,
        finish_hash: String,
        entries: impl IntoIterator<Item = Entry>,
    ) -> Result<Self, InvestigationError> {
        Self::collect_with_budget(
            refusal_hash,
            finish_hash,
            entries,
            MAX_REFUSAL_DIAGNOSTIC_BYTES,
        )
    }

    fn collect_with_budget(
        refusal_hash: String,
        finish_hash: String,
        entries: impl IntoIterator<Item = Entry>,
        budget: usize,
    ) -> Result<Self, InvestigationError> {
        Self::collect_visit(refusal_hash, finish_hash, budget, |emit| {
            for entry in entries {
                emit(entry)?;
            }
            Ok(())
        })
    }

    pub(super) fn collect_visit(
        refusal_hash: String,
        finish_hash: String,
        budget: usize,
        visit: impl FnOnce(
            &mut dyn FnMut(Entry) -> Result<(), InvestigationError>,
        ) -> Result<(), InvestigationError>,
    ) -> Result<Self, InvestigationError> {
        let mut result = Self {
            schema_version: 1,
            refusal_hash,
            finish_hash,
            retention: Retention::Recorded {
                entries: Vec::new(),
                omitted_entries: 0,
            },
        };
        let mut exhausted = false;
        visit(&mut |entry| {
            entry.validate()?;
            let Retention::Recorded {
                entries,
                omitted_entries,
            } = &mut result.retention
            else {
                unreachable!()
            };
            if exhausted {
                *omitted_entries = omitted_entries
                    .checked_add(1)
                    .ok_or(InvestigationError::LimitExceeded)?;
            } else {
                entries.push(entry);
            }
            while bounded_bytes(&result, budget).is_err() {
                let Retention::Recorded {
                    entries,
                    omitted_entries,
                } = &mut result.retention
                else {
                    unreachable!()
                };
                if entries.pop().is_none() {
                    break;
                }
                *omitted_entries = omitted_entries
                    .checked_add(1)
                    .ok_or(InvestigationError::LimitExceeded)?;
                exhausted = true;
            }
            Ok(())
        })?;
        if let Retention::Recorded {
            entries,
            omitted_entries,
        } = &result.retention
            && entries.is_empty()
            && *omitted_entries > 0
        {
            result.retention = Retention::Unrecorded {
                reason: UnrecordedReason::Capacity,
                omitted_entries: *omitted_entries,
            };
        }
        // The explicit marker uses the fixed sidecar allowance even if a test
        // or collector has no allowance for detailed entries.
        result.bytes()?;
        Ok(result)
    }

    pub fn bytes(&self) -> Result<Vec<u8>, InvestigationError> {
        if self.schema_version != 1
            || !crate::source_basis::hash(&self.refusal_hash)
            || !crate::source_basis::hash(&self.finish_hash)
        {
            return Err(InvestigationError::InvalidInput);
        }
        let bytes = bounded_bytes(self, MAX_REFUSAL_DIAGNOSTIC_BYTES)?;
        match &self.retention {
            Retention::Recorded { entries, .. } => {
                for entry in entries {
                    entry.validate()?;
                }
            }
            Retention::Unrecorded {
                reason: UnrecordedReason::Capacity,
                omitted_entries: 0,
            } => return Err(InvestigationError::InvalidInput),
            Retention::Unrecorded { .. } => {}
        }
        Ok(bytes)
    }

    pub fn content_hash(&self) -> Result<String, InvestigationError> {
        Ok(crate::source_basis::domain_hash(
            b"cartograph:refusal-diagnostic-sidecar:v1\0",
            &self.bytes()?,
        ))
    }
}

impl Entry {
    fn validate(&self) -> Result<(), InvestigationError> {
        self.location.validate()?;
        if let Detail::Enumeration { diagnostic } = &self.detail {
            for index in [
                diagnostic.explanation.target_token,
                diagnostic.explanation.quantifier_token,
            ]
            .into_iter()
            .flatten()
            {
                if index >= diagnostic.shape.tokens.len() {
                    return Err(InvestigationError::InvalidInput);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::investigation::{ProseField, ProsePosition};
    fn entry() -> Entry {
        let position = ProsePosition {
            field: ProseField::FindingStatement { finding: 0 },
            sentence: 1,
        };
        Entry {
            location: ReplayLocation {
                start: position.clone(),
                end: position,
            },
            citation_index: Some(0),
            detail: Detail::Enumeration {
                diagnostic: explain_enumeration("['A', 'B']", "All entries are A", 4096).unwrap(),
            },
        }
    }
    // AC-0287: rejected collection has an unknown count, unlike capacity loss.
    #[test]
    fn rejected_collector_marker_allows_unknown_count_only_for_rejection() {
        let mut sidecar = Sidecar {
            schema_version: 1,
            refusal_hash: "a".repeat(64),
            finish_hash: "b".repeat(64),
            retention: Retention::Unrecorded {
                reason: UnrecordedReason::CollectorRejected,
                omitted_entries: 0,
            },
        };
        assert!(sidecar.bytes().unwrap().len() <= MAX_REFUSAL_DIAGNOSTIC_BYTES);
        sidecar.retention = Retention::Unrecorded {
            reason: UnrecordedReason::Capacity,
            omitted_entries: 0,
        };
        assert!(sidecar.bytes().is_err());
    }

    // AC-0287 / #1707: full serialized envelope is capped; traversal continues.
    #[test]
    fn overflow_preserves_prefix_and_counts_every_omission() {
        let mut visited = 0;
        let sidecar = Sidecar::collect(
            "a".repeat(64),
            "b".repeat(64),
            (0..1000).map(|_| {
                visited += 1;
                entry()
            }),
        )
        .unwrap();
        assert_eq!(visited, 1000);
        assert!(sidecar.bytes().unwrap().len() <= MAX_REFUSAL_DIAGNOSTIC_BYTES);
        let Retention::Recorded {
            entries,
            omitted_entries,
        } = &sidecar.retention
        else {
            panic!("prefix fits")
        };
        assert!(!entries.is_empty());
        assert_eq!(entries.len() + omitted_entries, 1000);
        assert!(entries.iter().all(|saved| saved == &entry()));
        let roundtrip: Sidecar = serde_json::from_slice(&sidecar.bytes().unwrap()).unwrap();
        assert_eq!(roundtrip, sidecar);
    }
    #[test]
    fn no_detail_room_has_explicit_marker_and_stable_binding() {
        let sidecar =
            Sidecar::collect_with_budget("a".repeat(64), "b".repeat(64), [entry(), entry()], 0)
                .unwrap();
        assert_eq!(
            sidecar.retention,
            Retention::Unrecorded {
                reason: UnrecordedReason::Capacity,
                omitted_entries: 2
            }
        );
        let original = sidecar.content_hash().unwrap();
        let mut altered = sidecar.clone();
        altered.finish_hash = "c".repeat(64);
        assert_ne!(original, altered.content_hash().unwrap());
        altered.finish_hash = "arbitrary prose".into();
        assert!(altered.bytes().is_err());
    }
    #[test]
    fn dangling_shape_reference_is_rejected() {
        let mut candidate = entry();
        let Detail::Enumeration { diagnostic } = &mut candidate.detail else {
            unreachable!()
        };
        diagnostic.shape.tokens.clear();
        assert!(Sidecar::collect("a".repeat(64), "b".repeat(64), [candidate]).is_err());
    }
}
