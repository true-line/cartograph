//! Versioned progressive query disclosure. The durable ledger retains every
//! selector; this representation supplies one latest view and bounded references
//! to older acquisitions. References do not supply their selectors or results.
use super::*;
use crate::TaskFactKey;
use core_prov::content_hash;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const QUERY_HISTORY_CONTRACT: &str = "investigation-query-history@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryHistoryReference {
    pub query_index: usize,
    pub query_id: String,
}

impl QueryHistoryReference {
    /// Bind the complete admitted selector and response manifest to its request,
    /// context and acquisition position. No current graph is consulted.
    pub fn from_ledger(
        request_hash: &str,
        ledger: &InvestigationInputLedger,
        query_index: usize,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        validate_basis(request_hash, ledger, policy)?;
        reference(request_hash, ledger, query_index)
    }

    /// Resolve only the exact saved input selector. A host replay must still
    /// execute a normal counted query; this method returns no acquired page or
    /// invented continuation cursor and authorizes no provider dispatch.
    pub fn resolve(
        &self,
        request_hash: &str,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<InvestigationQuery, InvestigationError> {
        if self != &Self::from_ledger(request_hash, ledger, self.query_index, policy)? {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(ledger.queries[self.query_index].query.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryHistoryContext {
    pub snapshot_id: String,
    pub view: context_hub::ContextView,
    pub total_selected: usize,
    pub next_cursor: Option<InvestigationQueryCursor>,
    pub fact_indexes: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryHistoryView {
    pub reference: QueryHistoryReference,
    pub selection: InvestigationQuery,
    pub context: QueryHistoryContext,
    pub evidence_option_indexes: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationQueryHistory {
    pub contract: String,
    pub registry_hash: String,
    /// Ordered references only. Full prior selectors and result metadata are
    /// deliberately undisclosed; their complete manifests remain in the ledger.
    pub prior_queries: Vec<QueryHistoryReference>,
    pub latest_query: Option<QueryHistoryView>,
}

impl InvestigationQueryHistory {
    /// Validate the registry and the latest input selector against saved
    /// acquisition metadata. The original response bytes must independently
    /// validate the latest cursor, indexes and result before live sealing.
    pub fn validate_registry(
        &self,
        request_hash: &str,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        validate_basis(request_hash, ledger, policy)?;
        let references = references(request_hash, ledger)?;
        let latest = references.last();
        let prior_count = references.len().saturating_sub(1);
        if self.contract != QUERY_HISTORY_CONTRACT
            || self.registry_hash != registry_hash(request_hash, ledger, policy, &references)?
            || self.prior_queries != references[..prior_count]
            || self.latest_query.as_ref().map(|view| &view.reference) != latest
        {
            return Err(InvestigationError::InvalidInput);
        }
        if let Some(view) = &self.latest_query {
            let manifest = &ledger.queries[view.reference.query_index];
            let inventory_bound = ledger.queries.iter().try_fold(0usize, |count, query| {
                count
                    .checked_add(query.returned_facts)
                    .ok_or(InvestigationError::LimitExceeded)
            })?;
            if view.context.next_cursor.as_ref().is_some_and(|cursor| {
                cursor.snapshot_id != ledger.scope_snapshot_id
                    || !text(&cursor.selection_id, 128)
                    || cursor.offset > 50_000
            }) || view
                .context
                .fact_indexes
                .iter()
                .chain(&view.evidence_option_indexes)
                .any(|index| *index >= inventory_bound)
            {
                return Err(InvestigationError::InvalidInput);
            }
            if view.selection != manifest.query
                || view.context.snapshot_id != ledger.scope_snapshot_id
                || view.context.total_selected != manifest.total_selected
                || view.context.next_cursor.is_some() != manifest.has_more
                || view.context.fact_indexes.len() != manifest.returned_facts
                || view.evidence_option_indexes.len() != manifest.returned_facts
            {
                return Err(InvestigationError::InvalidInput);
            }
        }
        Ok(())
    }

    /// Exact bytes for this JSON value when encoded inside a JSON prompt string,
    /// including that string's two quotes. This is component accounting only.
    pub fn transport_bytes(&self) -> Result<usize, InvestigationError> {
        nested_json_bytes(self)
    }

    /// A conservative diagnostic bound derived from the existing typed field
    /// ceilings. It neither registers a new limit nor establishes payload fit.
    ///
    /// For one UTF-8 input byte, U+0001 dominates both JSON string encodings:
    /// `\u0001` becomes seven bytes after the prompt is itself JSON-escaped.
    /// Quotes/backslashes require four and other UTF-8 bytes at most one. Hash
    /// references remain exactly 64 unescaped ASCII bytes. Every variable string
    /// and number below dominates its admitted field; no selector deduplication
    /// or expected completion depth is assumed.
    pub fn transport_upper_bound_with_policy(
        policy: &InvestigationPolicy,
    ) -> Result<usize, InvestigationError> {
        require_policy(policy)?;
        let maximum = usize::try_from(policy.limits().tool_actions)
            .map_err(|_| InvestigationError::LimitExceeded)?;
        let last_index = maximum
            .checked_sub(1)
            .ok_or(InvestigationError::InvalidInput)?;
        let maximum_inventory_index = maximum
            .checked_mul(MAX_QUERY_FACTS)
            .and_then(|count| count.checked_sub(1))
            .ok_or(InvestigationError::LimitExceeded)?;
        let cursor = InvestigationQueryCursor {
            snapshot_id: "\u{1}".repeat(128),
            selection_id: "\u{1}".repeat(128),
            offset: 50_000,
        };
        let selector = InvestigationQuery {
            scope: InvestigationScope::Neighborhood {
                anchor: "\u{1}".repeat(1024),
                hops: 3,
            },
            kind: Some(InvestigationFactKind::Node),
            labels: vec!["\u{1}".repeat(128); 16],
            max_facts: MAX_QUERY_FACTS,
            max_bytes: MAX_QUERY_BYTES,
            cursor: Some(cursor.clone()),
        };
        selector.validate()?;
        let reference = QueryHistoryReference {
            query_index: last_index,
            query_id: "f".repeat(64),
        };
        // Using the largest index in every reference intentionally overcounts
        // early indices, and need not describe a reachable registry.
        Self {
            contract: QUERY_HISTORY_CONTRACT.into(),
            registry_hash: "f".repeat(64),
            prior_queries: vec![reference.clone(); last_index],
            latest_query: Some(QueryHistoryView {
                reference,
                selection: selector,
                context: QueryHistoryContext {
                    // Actual scoped context identities are shorter than this.
                    snapshot_id: "\u{1}".repeat(128),
                    view: context_hub::ContextView::RecoveredGraph,
                    total_selected: 50_000,
                    next_cursor: Some(cursor),
                    fact_indexes: vec![maximum_inventory_index; MAX_QUERY_FACTS],
                },
                evidence_option_indexes: vec![maximum_inventory_index; MAX_QUERY_FACTS],
            }),
        }
        .transport_bytes()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressiveInvestigationContext {
    /// Every complete acquired fact representation, sharing only exact equals.
    pub facts: Vec<Value>,
    /// Every complete acquired evidence inventory, sharing only exact equals.
    pub evidence_options: Vec<Value>,
    pub query_history: InvestigationQueryHistory,
}

impl ProgressiveInvestigationContext {
    /// Validate raw host pages directly. In particular this must never construct
    /// the old all-selectors/all-qualifications compact intermediate, whose cap
    /// may already reject a state that the progressive representation can hold.
    pub fn prepare(
        request_hash: &str,
        ledger: &InvestigationInputLedger,
        raw_pages: &[String],
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        Self::prepare_raw(request_hash, ledger, raw_pages, policy)
    }

    pub(crate) fn prepare_raw<P: AsRef<str>>(
        request_hash: &str,
        ledger: &InvestigationInputLedger,
        raw_pages: &[P],
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        validate_basis(request_hash, ledger, policy)?;
        if raw_pages.len() != ledger.queries.len() {
            return Err(InvestigationError::InvalidInput);
        }
        let proof = ledger
            .coverage
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        let public = serde_json::to_value(&proof.qualification)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let references = references(request_hash, ledger)?;
        let prior_count = references.len().saturating_sub(1);
        let mut result = Self {
            facts: vec![],
            evidence_options: vec![],
            query_history: InvestigationQueryHistory {
                contract: QUERY_HISTORY_CONTRACT.into(),
                registry_hash: registry_hash(request_hash, ledger, policy, &references)?,
                prior_queries: references[..prior_count].to_vec(),
                latest_query: None,
            },
        };
        let selected: BTreeSet<_> = ledger
            .selected_facts
            .iter()
            .map(|fact| &fact.fact)
            .collect();
        let mut observed = BTreeSet::new();
        let mut total_bytes = 0usize;
        for (index, (raw, manifest)) in raw_pages.iter().zip(&ledger.queries).enumerate() {
            let raw = raw.as_ref();
            if raw.len() != manifest.response_bytes
                || content_hash(raw.as_bytes()) != manifest.response_hash
            {
                return Err(InvestigationError::InvalidInput);
            }
            total_bytes = total_bytes
                .checked_add(raw.len())
                .ok_or(InvestigationError::LimitExceeded)?;
            if total_bytes > MAX_TOTAL_QUERY_BYTES {
                return Err(InvestigationError::LimitExceeded);
            }
            strict::preflight(raw, MAX_QUERY_BYTES, 32, 16384, 16384)?;
            let page: Value =
                serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidInput)?;
            exact_fields(&page, &["context", "evidence_options"])?;
            let qualified = &page["context"];
            exact_fields(qualified, &["context_id", "coverage", "response"])?;
            if qualified["context_id"].as_str() != Some(proof.context_id.as_str())
                || qualified["coverage"] != public
            {
                return Err(InvestigationError::InvalidInput);
            }
            let context = &qualified["response"];
            // Legacy compaction retains unknown metadata verbatim. This new
            // closed view instead rejects it; dropping future fields silently
            // would manufacture a supposedly complete summary.
            exact_fields(
                context,
                &[
                    "snapshot_id",
                    "view",
                    "total_selected",
                    "facts",
                    "next_cursor",
                ],
            )?;
            let facts = context["facts"]
                .as_array()
                .ok_or(InvestigationError::InvalidInput)?;
            let options = page["evidence_options"]
                .as_array()
                .ok_or(InvestigationError::InvalidInput)?;
            let next_cursor: Option<InvestigationQueryCursor> =
                serde_json::from_value(context["next_cursor"].clone())
                    .map_err(|_| InvestigationError::InvalidInput)?;
            if let Some(cursor) = &next_cursor
                && (cursor.snapshot_id != ledger.scope_snapshot_id
                    || !text(&cursor.selection_id, 128)
                    || cursor.offset > 50_000)
            {
                return Err(InvestigationError::InvalidInput);
            }
            let view = serde_json::from_value(context["view"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
            if context["snapshot_id"].as_str() != Some(ledger.scope_snapshot_id.as_str())
                || context["total_selected"].as_u64() != Some(manifest.total_selected as u64)
                || next_cursor.is_some() != manifest.has_more
                || facts.len() != manifest.returned_facts
                || options.len() != facts.len()
            {
                return Err(InvestigationError::InvalidInput);
            }
            let mut fact_indexes = Vec::new();
            let mut option_indexes = Vec::new();
            let mut page_keys = BTreeSet::new();
            for (fact, option) in facts.iter().zip(options) {
                let key = key_at(fact, "reference")?;
                if !selected.contains(&key)
                    || key_at(option, "fact")? != key
                    || !page_keys.insert(key.clone())
                {
                    return Err(InvestigationError::InvalidInput);
                }
                observed.insert(key);
                fact_indexes.push(intern(&mut result.facts, fact));
                option_indexes.push(intern(&mut result.evidence_options, option));
            }
            if index == prior_count {
                result.query_history.latest_query = Some(QueryHistoryView {
                    reference: references[index].clone(),
                    selection: manifest.query.clone(),
                    context: QueryHistoryContext {
                        snapshot_id: ledger.scope_snapshot_id.clone(),
                        view,
                        total_selected: manifest.total_selected,
                        next_cursor,
                        fact_indexes,
                    },
                    evidence_option_indexes: option_indexes,
                });
            }
        }
        if observed.iter().collect::<BTreeSet<_>>() != selected {
            return Err(InvestigationError::InvalidInput);
        }
        result
            .query_history
            .validate_registry(request_hash, ledger, policy)?;
        bounded_bytes(&result, MAX_INPUT_BYTES)?;
        Ok(result)
    }
}

fn require_policy(policy: &InvestigationPolicy) -> Result<(), InvestigationError> {
    if policy.profile() != INVESTIGATION_V2_POLICY.profile() {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(())
}

fn validate_basis(
    request_hash: &str,
    ledger: &InvestigationInputLedger,
    policy: &InvestigationPolicy,
) -> Result<(), InvestigationError> {
    require_policy(policy)?;
    if !crate::source_basis::hash(request_hash)
        || ledger.schema_version != 2
        || ledger.coverage.is_none()
    {
        return Err(InvestigationError::InvalidInput);
    }
    ledger.validate_with_policy(policy)
}

fn reference(
    request_hash: &str,
    ledger: &InvestigationInputLedger,
    query_index: usize,
) -> Result<QueryHistoryReference, InvestigationError> {
    let manifest = ledger
        .queries
        .get(query_index)
        .ok_or(InvestigationError::InvalidInput)?;
    let proof = ledger
        .coverage
        .as_ref()
        .ok_or(InvestigationError::InvalidInput)?;
    Ok(QueryHistoryReference {
        query_index,
        query_id: crate::source_basis::domain_hash(
            b"cartograph:query-history-entry:v1\0",
            &bounded_bytes(
                &(
                    request_hash,
                    &ledger.graph_snapshot_id,
                    &ledger.scope_snapshot_id,
                    &proof.context_id,
                    query_index,
                    manifest,
                ),
                MAX_MANIFEST_BYTES,
            )?,
        ),
    })
}

fn references(
    request_hash: &str,
    ledger: &InvestigationInputLedger,
) -> Result<Vec<QueryHistoryReference>, InvestigationError> {
    (0..ledger.queries.len())
        .map(|index| reference(request_hash, ledger, index))
        .collect()
}

pub(super) fn registry_hash(
    request_hash: &str,
    ledger: &InvestigationInputLedger,
    policy: &InvestigationPolicy,
    references: &[QueryHistoryReference],
) -> Result<String, InvestigationError> {
    Ok(crate::source_basis::domain_hash(
        b"cartograph:query-history-registry:v1\0",
        &bounded_bytes(
            &(
                request_hash,
                ledger.fingerprint_with_policy(policy)?,
                references,
            ),
            MAX_MANIFEST_BYTES,
        )?,
    ))
}

fn exact_fields(value: &Value, fields: &[&str]) -> Result<(), InvestigationError> {
    let object = value.as_object().ok_or(InvestigationError::InvalidInput)?;
    if object.len() != fields.len() || fields.iter().any(|field| !object.contains_key(*field)) {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(())
}

fn key_at(value: &Value, field: &str) -> Result<TaskFactKey, InvestigationError> {
    serde_json::from_value(
        value
            .get(field)
            .cloned()
            .ok_or(InvestigationError::InvalidInput)?,
    )
    .map_err(|_| InvestigationError::InvalidInput)
}

fn intern(inventory: &mut Vec<Value>, value: &Value) -> usize {
    if let Some(index) = inventory.iter().position(|prior| prior == value) {
        index
    } else {
        inventory.push(value.clone());
        inventory.len() - 1
    }
}

fn nested_json_bytes(value: &impl Serialize) -> Result<usize, InvestigationError> {
    let raw = String::from_utf8(bounded_bytes(value, MAX_INPUT_BYTES)?)
        .map_err(|_| InvestigationError::InvalidInput)?;
    serde_json::to_vec(&raw)
        .map(|bytes| bytes.len())
        .map_err(|_| InvestigationError::InvalidInput)
}

#[cfg(test)]
#[path = "query_history_tests.rs"]
mod tests;
