//! Finding-local projection from the exact authenticated input and finish.
use super::claim_diagnostics::ClaimDiagnostic;
use super::diagnostic_sidecar::{Detail, Entry, MAX_REFUSAL_DIAGNOSTIC_BYTES, Sidecar};
use super::*;

impl InvestigationInput {
    /// The host supplies the admitted refusal identity. Collection never modifies
    /// the finish, input ledger, feedback or detector verdicts.
    pub fn refusal_diagnostic_sidecar(
        &self,
        finish: &staged_finish::StagedFinish,
        refusal_hash: String,
    ) -> Result<Sidecar, InvestigationError> {
        let InvestigationAction::Finish { findings, .. } = &finish.action else {
            return Err(InvestigationError::InvalidAction);
        };
        Sidecar::collect_visit(
            refusal_hash,
            finish.fingerprint()?,
            MAX_REFUSAL_DIAGNOSTIC_BYTES,
            |emit| {
                for (finding_index, finding) in findings.iter().enumerate() {
                    let cited: Vec<_> = self
                        .payload
                        .spans
                        .iter()
                        .filter_map(|span| {
                            finding
                                .citation_ids
                                .iter()
                                .position(|id| id == &span.id)
                                .map(|ordinal| (ordinal, span))
                        })
                        .collect();
                    let spans: Vec<_> = cited
                        .iter()
                        .map(|(_, span)| (span.path.as_str(), span.text.as_str()))
                        .collect();
                    let mut failure = None;
                    super::enumeration_claims::visit_diagnostics_with_grammar(
                        &finding.title,
                        &finding.statement,
                        &spans,
                        MAX_REFUSAL_DIAGNOSTIC_BYTES,
                        self.frozen_specialist()? == SpecialistId::DomainAnalystV27,
                        |title, sentence, diagnostic| {
                            let position = ProsePosition {
                                field: if title {
                                    ProseField::FindingTitle {
                                        finding: finding_index as u32,
                                    }
                                } else {
                                    ProseField::FindingStatement {
                                        finding: finding_index as u32,
                                    }
                                },
                                sentence: sentence as u32,
                            };
                            let mut retain = |span: Option<usize>, detail| {
                                let citation_index = match span {
                                    Some(index) => match cited.get(index) {
                                        Some((ordinal, _)) => Some(*ordinal),
                                        None => {
                                            failure = Some(InvestigationError::InvalidInput);
                                            return;
                                        }
                                    },
                                    None => None,
                                };
                                if let Err(error) = emit(Entry {
                                    location: ReplayLocation {
                                        start: position.clone(),
                                        end: position.clone(),
                                    },
                                    citation_index,
                                    detail,
                                }) {
                                    failure = Some(error);
                                }
                            };
                            match diagnostic {
                                ClaimDiagnostic::Enumeration { span, diagnostic } => {
                                    retain(Some(span), Detail::Enumeration { diagnostic })
                                }
                                ClaimDiagnostic::Configuration {
                                    predicate_token,
                                    failures,
                                } => {
                                    if failures.is_empty() {
                                        retain(None, Detail::Configuration { predicate_token, reason: claim_diagnostics::ConfigurationFailure::SubjectUnbound });
                                    }
                                    for (span, reason) in failures {
                                        retain(
                                            Some(span),
                                            Detail::Configuration {
                                                predicate_token,
                                                reason,
                                            },
                                        );
                                    }
                                }
                            }
                        },
                    );
                    if let Some(error) = failure {
                        return Err(error);
                    }
                }
                Ok(())
            },
        )
    }
}
