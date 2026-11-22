use super::{
    AcquisitionSink, HostError,
    context::{EvidenceOption, EvidenceOptions, FrozenContext, FrozenFact, ScopedGraph},
};
use crate::{
    AppState,
    primary_source::{PinnedReadError, RetentionGuard},
    sources::RegisteredSource,
};
use adapters_lang_ts::captured::{RangeRole, Receipt};
use agents::investigation::*;
use agents::{
    AgentEvidence, InputClosureStatus, PrimarySourceScope, TaskCaptureFileRef, TaskCaptureSpanRef,
    TaskFactKey, TaskFactSelection, TaskRangeRole,
};
use core_graph::source::FactKey;
use core_prov::{EvidenceRef, Provenance, content_hash};
use std::collections::BTreeMap;

#[path = "prepared_source.rs"]
pub(super) mod prepared_source;

pub(super) struct EvidenceGuards {
    sources: BTreeMap<String, RegisteredSource>,
    guards: BTreeMap<String, RetentionGuard>,
    references: Vec<InvestigationReceiptReference>,
}

impl EvidenceGuards {
    /// Validate saved registration identities through the active state.db
    /// writer and verify the already-held retention handles. Acquires no new
    /// OS locks or registry/jobs mutexes and makes no live-graph freshness claim.
    pub(super) fn revalidate_in_view(
        &self,
        primary: &crate::primary_source::PrimarySourceStore,
        view: &crate::sources::RetainedSourceView<'_>,
    ) -> Result<(), HostError> {
        for expected in self.sources.values() {
            view.verify(expected).map_err(|error| match error {
                crate::sources::RetainedSourceError::Missing => HostError::SourceUnavailable,
                crate::sources::RetainedSourceError::Changed => HostError::InputChanged,
                crate::sources::RetainedSourceError::Operational => HostError::Operational,
            })?;
        }
        for guard in self.guards.values() {
            primary.verify_task_guard(guard).map_err(pinned_error)?;
        }
        Ok(())
    }

    pub(super) fn acquire(
        state: &AppState,
        selections: &BTreeMap<FactKey, TaskFactSelection>,
    ) -> Result<Self, HostError> {
        let mut sources = BTreeMap::new();
        let mut references = Vec::new();
        for selected in selections.values() {
            let Some(binding) = &selected.binding else {
                continue;
            };
            if let std::collections::btree_map::Entry::Vacant(entry) =
                sources.entry(binding.repo_key.clone())
            {
                let source = state
                    .sources
                    .lock()
                    .map_err(|_| HostError::Operational)?
                    .get_by_repo(&binding.repo_key)
                    .map_err(|_| HostError::Operational)?
                    .ok_or(HostError::SourceUnavailable)?;
                entry.insert(source);
            }
            let source = sources
                .get(&binding.repo_key)
                .ok_or(HostError::InvalidInput)?;
            references.push(InvestigationReceiptReference {
                source_id: source.source_id.clone(),
                repo_key: binding.repo_key.clone(),
                receipt_id: binding.receipt_id.clone(),
            });
        }
        references.sort();
        references.dedup();
        let mut guards = BTreeMap::new();
        for source_id in sources
            .values()
            .map(|s| s.source_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
        {
            guards.insert(
                source_id.clone(),
                state
                    .primary_sources
                    .task_guard(&source_id)
                    .map_err(pinned_error)?,
            );
        }
        Ok(Self {
            sources,
            guards,
            references,
        })
    }

    pub(super) fn references(&self) -> Vec<InvestigationReceiptReference> {
        self.references.clone()
    }

    fn receipt(
        &self,
        state: &AppState,
        selected: &TaskFactSelection,
    ) -> Result<Receipt, HostError> {
        let binding = selected.binding.as_ref().ok_or(HostError::InvalidInput)?;
        let source = self
            .sources
            .get(&binding.repo_key)
            .ok_or(HostError::InvalidInput)?;
        let guard = self
            .guards
            .get(&source.source_id)
            .ok_or(HostError::InvalidInput)?;
        state
            .primary_sources
            .task_receipt(
                guard,
                &crate::task_evidence::graph_fact(&selected.fact),
                &selected.fact_digest,
                &binding.repo_key,
                &binding.receipt_id,
            )
            .map_err(pinned_error)
    }

    /// Recheck already-held retention identities and registered source metadata.
    /// Captured evidence remains independent of checkout availability/freshness.
    fn revalidate(&self, state: &AppState) -> Result<(), HostError> {
        for (repo, expected) in &self.sources {
            let current = state
                .sources
                .lock()
                .map_err(|_| HostError::Operational)?
                .get_by_repo(repo)
                .map_err(|_| HostError::Operational)?
                .ok_or(HostError::SourceUnavailable)?;
            if current.source_id != expected.source_id
                || current.repo_key != expected.repo_key
                || current.root() != expected.root()
                || current.is_managed() != expected.is_managed()
                || current.clone_url() != expected.clone_url()
            {
                return Err(HostError::InputChanged);
            }
        }
        for guard in self.guards.values() {
            state
                .primary_sources
                .verify_task_guard(guard)
                .map_err(pinned_error)?;
        }
        Ok(())
    }

    pub(super) fn options(
        &self,
        state: &AppState,
        context: &FrozenContext,
        fresh: &ScopedGraph,
        selected: &TaskFactSelection,
    ) -> Result<EvidenceOptions, HostError> {
        let (origin, ranges) = if selected.binding.is_some() {
            let receipt = self.receipt(state, selected)?;
            validate_receipt_fact(&receipt, context, fresh, &selected.fact)?;
            (
                "captured_primary_source_metadata",
                receipt
                    .ranges()
                    .iter()
                    .map(|r| EvidenceOption {
                        role: role(r.role),
                        index: r.index,
                        source: r.evidence.clone(),
                    })
                    .collect(),
            )
        } else {
            (
                "working_tree_unverified",
                legacy_references(context.props_in(&selected.fact, fresh)?)
                    .into_iter()
                    .enumerate()
                    .map(|(index, source)| EvidenceOption {
                        role: TaskRangeRole::Provenance,
                        index: index as u32,
                        source,
                    })
                    .collect(),
            )
        };
        Ok(EvidenceOptions {
            fact: selected.fact.clone(),
            origin,
            ranges,
        })
    }
}

#[cfg(test)]
#[path = "evidence_retained_view_tests.rs"]
mod retained_view_tests;

impl FrozenContext {
    pub(super) fn read_evidence(
        &mut self,
        state: &AppState,
        fact: TaskFactKey,
        wanted_role: TaskRangeRole,
        index: u32,
        sink: &mut impl AcquisitionSink,
    ) -> Result<(), HostError> {
        let selected = self
            .ledger
            .selected_facts
            .iter()
            .find(|s| s.fact == fact)
            .cloned()
            .ok_or(HostError::InvalidInput)?;
        if index >= 1024 || self.evidence.len() >= self.policy.limits().evidence_items {
            return Err(HostError::LimitExceeded);
        }
        let keys = self
            .ledger
            .selected_facts
            .iter()
            .map(|s| crate::task_evidence::graph_fact(&s.fact))
            .collect::<Vec<_>>();
        let preliminary = self.select(state, &keys)?;
        let guards = EvidenceGuards::acquire(state, &preliminary)?;
        // Legacy source operations have their own registered checkout authority.
        // Acquire it before the authoritative union recheck and keep it through publication.
        let legacy_source = if selected.binding.is_none() {
            if wanted_role != TaskRangeRole::Provenance {
                return Err(HostError::SourceUnavailable);
            }
            Some(
                legacy_references(self.props(&fact)?)
                    .get(index as usize)
                    .cloned()
                    .ok_or(HostError::SourceUnavailable)?,
            )
        } else {
            None
        };
        let legacy_operation = if let Some(reference) = &legacy_source {
            let source = state
                .sources
                .lock()
                .map_err(|_| HostError::Operational)?
                .get_by_repo(&reference.repo)
                .map_err(|_| HostError::Operational)?
                .ok_or(HostError::SourceUnavailable)?;
            Some(
                crate::source_access::SourceOperation::acquire(&state.sources, [(source, false)])
                    .map_err(|_| HostError::Operational)?,
            )
        } else {
            None
        };
        if self.select(state, &keys)? != preliminary {
            return Err(HostError::InputChanged);
        }
        let (source, text, origin) = if let Some(binding) = &selected.binding {
            let receipt = guards.receipt(state, &selected)?;
            validate_receipt_fact(&receipt, self, super::context::NO_ROWS, &fact)?;
            let (inventory_index, range) = receipt
                .ranges()
                .iter()
                .enumerate()
                .find(|(_, r)| role(r.role) == wanted_role && r.index == index)
                .ok_or(HostError::InvalidInput)?;
            let span_len = range
                .captured
                .byte_end
                .checked_sub(range.captured.byte_start)
                .filter(|n| *n > 0 && *n <= 8192)
                .ok_or(HostError::LimitExceeded)?;
            if sink
                .usage()
                .evidence_bytes
                .checked_add(span_len as usize)
                .is_none_or(|n| n > self.policy.limits().evidence_bytes)
            {
                return Err(HostError::LimitExceeded);
            }
            sink.reserve_validation(range.captured.file.byte_len)?;
            let registered = guards
                .sources
                .get(&binding.repo_key)
                .ok_or(HostError::InvalidInput)?;
            let guard = guards
                .guards
                .get(&registered.source_id)
                .ok_or(HostError::InvalidInput)?;
            let text = state
                .primary_sources
                .task_text(guard, &receipt, inventory_index)
                .map_err(pinned_error)?;
            if text.len() as u64 != span_len {
                return Err(HostError::InvalidInput);
            }
            (
                range.evidence.clone(),
                text,
                captured_origin(registered, &receipt, inventory_index)?,
            )
        } else {
            let reference = legacy_source.ok_or(HostError::InvalidInput)?;
            let length = reference
                .byte_end
                .checked_sub(reference.byte_start)
                .filter(|n| *n > 0 && *n <= 8192)
                .ok_or(HostError::LimitExceeded)?;
            if sink
                .usage()
                .evidence_bytes
                .checked_add(length as usize)
                .is_none_or(|n| n > self.policy.limits().evidence_bytes)
            {
                return Err(HostError::LimitExceeded);
            }
            let operation = legacy_operation.as_ref().ok_or(HostError::InvalidInput)?;
            let root = operation
                .root(&reference.repo)
                .map_err(|_| HostError::Operational)?;
            let text = crate::evidence::read_span_exact(
                root,
                &reference.path,
                &(reference.byte_start..reference.byte_end),
            )
            .map_err(|_| HostError::SourceUnavailable)?;
            if text.len() as u64 != length {
                return Err(HostError::InvalidInput);
            }
            (
                reference,
                text,
                InvestigationEvidenceOrigin::WorkingTreeUnverified,
            )
        };
        let (ledger, evidence) = append_source_evidence(
            self,
            fact,
            selected.fact_digest,
            wanted_role,
            index,
            source,
            text,
            origin,
        )?;
        let mut usage = sink.usage().clone();
        usage.evidence_items += 1;
        usage.evidence_bytes += evidence.text.len();
        sink.publish_input(&ledger, usage)?;
        self.ledger = ledger;
        self.evidence.push(evidence);
        Ok(())
    }
}

fn captured_origin(
    registered: &RegisteredSource,
    receipt: &Receipt,
    inventory_index: usize,
) -> Result<InvestigationEvidenceOrigin, HostError> {
    let range = receipt
        .ranges()
        .get(inventory_index)
        .ok_or(HostError::InvalidInput)?;
    let file = &range.captured.file;
    Ok(InvestigationEvidenceOrigin::CapturedPrimarySource {
        registered_source_id: registered.source_id.clone(),
        receipt_id: receipt.id().into(),
        receipt_inventory_index: u32::try_from(inventory_index)
            .map_err(|_| HostError::InvalidInput)?,
        captured: TaskCaptureSpanRef {
            file: TaskCaptureFileRef {
                source_id: file.source_id.as_str().into(),
                capture_id: file.capture_id.clone(),
                path: file.path.clone(),
                digest: file.digest.clone(),
                byte_len: file.byte_len,
            },
            byte_start: range.captured.byte_start,
            byte_end: range.captured.byte_end,
        },
        scope: PrimarySourceScope::PrimarySourceOnly,
        input_closure: InputClosureStatus::InputClosureNotEstablished,
    })
}

#[allow(clippy::too_many_arguments)]
fn append_source_evidence(
    context: &FrozenContext,
    fact: TaskFactKey,
    fact_digest: String,
    role: TaskRangeRole,
    index: u32,
    source: EvidenceRef,
    text: String,
    origin: InvestigationEvidenceOrigin,
) -> Result<(InvestigationInputLedger, AgentEvidence), HostError> {
    let id = format!("evidence-{}", context.evidence.len() + 1);
    let mut ledger = context.ledger.clone();
    ledger.revision = ledger
        .revision
        .checked_add(1)
        .ok_or(HostError::LimitExceeded)?;
    ledger.citations.push(InvestigationCitation {
        citation_id: id.clone(),
        fact,
        fact_digest,
        source: Some(source.clone()),
        role: Some(role),
        index: Some(index),
        text_hash: Some(content_hash(text.as_bytes())),
        origin,
    });
    ledger.validate_with_policy(&context.policy)?;
    Ok((ledger, AgentEvidence { id, source, text }))
}

fn legacy_references(props: &serde_json::Value) -> Vec<EvidenceRef> {
    props
        .get("prov")
        .cloned()
        .and_then(|p| serde_json::from_value::<Provenance>(p).ok())
        .filter(|p| p.validate().is_ok())
        .map(|p| p.evidence)
        .unwrap_or_default()
}
fn validate_receipt_fact(
    receipt: &Receipt,
    context: &FrozenContext,
    fresh: &ScopedGraph,
    fact: &TaskFactKey,
) -> Result<(), HostError> {
    let valid = match context.fact_in(fact, fresh) {
        Some(FrozenFact::Node(node)) => receipt.matches_node(node),
        Some(FrozenFact::Edge(edge)) => receipt.matches_edge(edge),
        None => false,
    };
    if valid {
        Ok(())
    } else {
        Err(HostError::InvalidInput)
    }
}
fn pinned_error(error: PinnedReadError) -> HostError {
    match error {
        PinnedReadError::Unavailable => HostError::SourceUnavailable,
        PinnedReadError::Invalid => HostError::InvalidInput,
        PinnedReadError::Operational => HostError::Operational,
    }
}
pub(super) fn role(value: RangeRole) -> TaskRangeRole {
    match value {
        RangeRole::Provenance => TaskRangeRole::Provenance,
        RangeRole::RuleExit => TaskRangeRole::RuleExit,
        RangeRole::ConditionBranch => TaskRangeRole::ConditionBranch,
        RangeRole::ConditionExpression => TaskRangeRole::ConditionExpression,
        RangeRole::ReturnValue => TaskRangeRole::ReturnValue,
        RangeRole::ThrowValue => TaskRangeRole::ThrowValue,
        RangeRole::Dependency => TaskRangeRole::Dependency,
        RangeRole::DependencyDeclaration => TaskRangeRole::DependencyDeclaration,
        RangeRole::Redaction => TaskRangeRole::Redaction,
        RangeRole::DefinitionDeclaration => TaskRangeRole::DefinitionDeclaration,
        RangeRole::DefinitionUse => TaskRangeRole::DefinitionUse,
        RangeRole::DefinitionInitializer => TaskRangeRole::DefinitionInitializer,
        RangeRole::DefinitionExpression => TaskRangeRole::DefinitionExpression,
        RangeRole::DefinitionDependency => TaskRangeRole::DefinitionDependency,
        RangeRole::DefinitionDependencyDeclaration => {
            TaskRangeRole::DefinitionDependencyDeclaration
        }
    }
}
