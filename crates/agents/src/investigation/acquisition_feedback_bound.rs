//! Serializer-only bound for the finite correction actions of one contract.
use super::*;

pub(in crate::investigation) fn feedback_encoded_bound(
    contract: &InvestigationCompletionContract,
    ledger: &InvestigationInputLedger,
    policy: &InvestigationPolicy,
) -> Result<usize, InvestigationError> {
    contract.validate_with_policy(policy)?;
    let context = &ledger
        .coverage
        .as_ref()
        .ok_or(InvestigationError::InvalidInput)?
        .context_id;
    let cap = staged_finish::RefusalFeedback::max_bytes_with_policy(policy)?;
    let mut entries = Vec::new();
    let mut entry_bytes = 0usize;
    for obligation in &contract.obligations.obligations {
        let Obligation::Source {
            obligation_id,
            target,
        } = obligation
        else {
            continue;
        };
        let entry = |action| CitationCorrectionEntry {
            target_id: target.target_id.clone(),
            obligation_id: obligation_id.clone(),
            action,
        };
        let read = entry(AcquisitionAction::ReadEvidence {
            fact: target.fact.clone(),
            role: target.role,
            index: target.index,
        });
        let chosen = if let TaskFactKey::Node { id } = &target.fact {
            let query = entry(AcquisitionAction::QueryContext {
                context_id: context.clone(),
                query: InvestigationQuery {
                    scope: InvestigationScope::Node { id: id.clone() },
                    kind: Some(InvestigationFactKind::Node),
                    labels: vec![],
                    max_facts: 1,
                    max_bytes: MAX_QUERY_BYTES,
                    cursor: None,
                },
            });
            // This is an encoded-byte bound, not a selected action. The final
            // backslash envelope separately dominates arbitrary escaping.
            match (bounded_bytes(&read, cap), bounded_bytes(&query, cap)) {
                (Ok(r), Ok(q)) => {
                    if q.len() > r.len() {
                        query
                    } else {
                        read
                    }
                }
                _ => return Ok(cap),
            }
        } else {
            read
        };
        let Ok(bytes) = bounded_bytes(&chosen, cap) else {
            return Ok(cap);
        };
        entry_bytes = entry_bytes
            .checked_add(bytes.len())
            .ok_or(InvestigationError::LimitExceeded)?;
        if entry_bytes > cap {
            return Ok(cap);
        }
        entries.push(chosen);
    }
    let feedback = staged_finish::RefusalFeedback {
        schema_version: 2,
        citation_correction: Some(CitationCorrection {
            schema_version: 1,
            choices_hash: "f".repeat(64),
            entries,
        }),
        unmet: contract
            .obligations
            .obligations
            .iter()
            .map(|item| {
                let (kind, id) = item.key();
                staged_finish::FeedbackEntry {
                    obligation_id: id.into(),
                    kind: kind.into(),
                    reason: staged_finish::RefusalReason::AcquiredNotSupplied,
                }
            })
            .collect(),
    };
    // All sources and unmet obligations dominate every admitted subset. Both
    // possible source actions use frozen contract identities and context;
    // acquisition cannot create another correction action shape. Never expose
    // this synthetic combination as feedback or use it to authorize anything.
    Ok(bounded_bytes(&feedback, cap).map_or(cap, |bytes| bytes.len()))
}
