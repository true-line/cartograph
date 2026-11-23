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
    let mut feedback = staged_finish::RefusalFeedback {
        citation_drop: None,
        prose_correction: None,
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
    let citation_bytes = bounded_bytes(&feedback, cap).map_or(cap, |bytes| bytes.len());
    feedback.schema_version = 3;
    feedback.citation_correction = None;
    feedback.prose_correction = Some(
        staged_finish::ProseCorrection::LimitationDeniesAcquiredEvidence(Box::new(
            staged_finish::ConflictingEvidence {
                evidence_ids: (1..=policy.limits().evidence_items)
                    .map(|i| format!("evidence-{i}"))
                    .collect(),
            },
        )),
    );
    let prose_bytes = bounded_bytes(&feedback, cap).map_or(cap, |bytes| bytes.len());
    Ok(citation_bytes.max(prose_bytes))
}

/// Serializer-only superset of every `turn_progress.citation_correction`
/// projection this contract can produce (#1653).
///
/// A real projection holds, for a subset of the contract's source obligations
/// in contract order, one entry whose action is the frozen choice for that
/// obligation: either `read_evidence` on the pinned target or, for a node
/// target not yet bound to a receipt, the exact one-fact node query. This value
/// lists every source obligation with every action it can take, under a
/// fixed-width choices hash, so any real projection serializes as a
/// subsequence of it. JSON escaping is per character, so the bound holds at
/// every enclosing string-embedding layer, and identities that need escaping
/// are measured as they actually are. The value is never decoded, exposed as
/// guidance, or used to authorize anything.
pub(in crate::investigation) fn citation_projection_upper(
    contract: &InvestigationCompletionContract,
    ledger: &InvestigationInputLedger,
    policy: &InvestigationPolicy,
) -> Result<serde_json::Value, InvestigationError> {
    contract.validate_with_policy(policy)?;
    let context = &ledger
        .coverage
        .as_ref()
        .ok_or(InvestigationError::InvalidInput)?
        .context_id;
    let mut entries = Vec::new();
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
        entries.push(entry(AcquisitionAction::ReadEvidence {
            fact: target.fact.clone(),
            role: target.role,
            index: target.index,
        }));
        if let TaskFactKey::Node { id } = &target.fact {
            entries.push(entry(AcquisitionAction::QueryContext {
                context_id: context.clone(),
                query: InvestigationQuery {
                    scope: InvestigationScope::Node { id: id.clone() },
                    kind: Some(InvestigationFactKind::Node),
                    labels: vec![],
                    max_facts: 1,
                    max_bytes: MAX_QUERY_BYTES,
                    cursor: None,
                },
            }));
        }
    }
    serde_json::to_value(Some(CitationCorrection {
        schema_version: 1,
        choices_hash: "f".repeat(64),
        entries,
    }))
    .map_err(|_| InvestigationError::InvalidInput)
}

/// Encoded length of `value` and of its next two JSON-string embeddings.
pub(in crate::investigation) fn embedded_lengths(
    value: &serde_json::Value,
) -> Result<[usize; 3], InvestigationError> {
    let one = serde_json::to_string(value).map_err(|_| InvestigationError::InvalidInput)?;
    let two = serde_json::to_string(&one).map_err(|_| InvestigationError::InvalidInput)?;
    let three = serde_json::to_string(&two).map_err(|_| InvestigationError::InvalidInput)?;
    Ok([one.len(), two.len(), three.len()])
}
