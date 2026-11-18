//! Lossless compaction of verified host query pages. Only identical complete
//! representations are shared; differing provenance or properties stay visible.
use super::*;
use crate::TaskFactKey;
use core_prov::content_hash;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

const MAX_QUERY_DEPTH: usize = 32;
const MAX_QUERY_VALUES: usize = 16384;
const MAX_QUERY_ARRAY_ITEMS: usize = 16384;

/// Compacted, model-facing view of verified query pages.
#[derive(Debug, Serialize)]
pub struct CompactInvestigationContext {
    /// Complete fact objects, including all original provenance and properties.
    pub facts: Vec<Value>,
    /// Complete host inventories; indexes/roles remain those of acquisition.
    pub evidence_options: Vec<Value>,
    /// Original page metadata with ordered indexes into the two inventories.
    pub queries: Vec<Value>,
}

impl CompactInvestigationContext {
    /// Compacts `raw_pages` under the v1 investigation policy.
    pub fn prepare(
        ledger: &InvestigationInputLedger,
        raw_pages: &[String],
    ) -> Result<Self, InvestigationError> {
        Self::prepare_with_policy(ledger, raw_pages, &INVESTIGATION_V1_POLICY)
    }
    /// Compacts `raw_pages` under an explicit policy.
    pub fn prepare_with_policy(
        ledger: &InvestigationInputLedger,
        raw_pages: &[String],
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        Self::prepare_raw_with_policy(ledger, raw_pages, policy)
    }
    pub(crate) fn prepare_raw_with_policy<P: AsRef<str>>(
        ledger: &InvestigationInputLedger,
        raw_pages: &[P],
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        ledger.validate_with_policy(policy)?;
        if raw_pages.len() != ledger.queries.len() {
            return Err(InvestigationError::InvalidInput);
        }
        let mut result = Self {
            facts: vec![],
            evidence_options: vec![],
            queries: vec![],
        };
        let selected: BTreeSet<_> = ledger.selected_facts.iter().map(|f| &f.fact).collect();
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
            strict::preflight(
                raw,
                MAX_QUERY_BYTES,
                MAX_QUERY_DEPTH,
                MAX_QUERY_VALUES,
                MAX_QUERY_ARRAY_ITEMS,
            )?;
            let mut page: Value =
                serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidInput)?;
            let envelope = page
                .as_object_mut()
                .ok_or(InvestigationError::InvalidInput)?;
            // Fail closed on a changed host envelope rather than discard fields.
            if envelope.len() != 2 {
                return Err(InvestigationError::InvalidInput);
            }
            let options = envelope
                .remove("evidence_options")
                .and_then(|value| match value {
                    Value::Array(values) => Some(values),
                    _ => None,
                })
                .ok_or(InvestigationError::InvalidInput)?;
            let context_value = envelope
                .get_mut("context")
                .ok_or(InvestigationError::InvalidInput)?;
            let qualification = if let Some(proof) = &ledger.coverage {
                let qualified = context_value
                    .as_object_mut()
                    .ok_or(InvestigationError::InvalidInput)?;
                if qualified.len() != 3
                    || qualified.get("context_id").and_then(Value::as_str)
                        != Some(proof.context_id.as_str())
                    || qualified.get("coverage")
                        != Some(
                            &serde_json::to_value(&proof.qualification)
                                .map_err(|_| InvestigationError::InvalidInput)?,
                        )
                {
                    return Err(InvestigationError::InvalidInput);
                }
                Some(json!({"context_id": proof.context_id, "coverage": proof.qualification}))
            } else {
                None
            };
            let context = if qualification.is_some() {
                context_value.get_mut("response")
            } else {
                Some(context_value)
            }
            .and_then(Value::as_object_mut)
            .ok_or(InvestigationError::InvalidInput)?;
            if context.contains_key("fact_indexes") {
                return Err(InvestigationError::InvalidInput);
            }
            let facts = context
                .remove("facts")
                .and_then(|value| match value {
                    Value::Array(values) => Some(values),
                    _ => None,
                })
                .ok_or(InvestigationError::InvalidInput)?;
            if context.get("snapshot_id").and_then(Value::as_str)
                != Some(ledger.scope_snapshot_id.as_str())
                || context.get("total_selected").and_then(Value::as_u64)
                    != Some(manifest.total_selected as u64)
                || !context.contains_key("next_cursor")
                || context.get("next_cursor").is_some_and(|v| !v.is_null()) != manifest.has_more
                || facts.len() != manifest.returned_facts
                || options.len() != facts.len()
            {
                return Err(InvestigationError::InvalidInput);
            }
            let mut fact_indexes = Vec::new();
            let mut option_indexes = Vec::new();
            let mut page_keys = BTreeSet::new();
            for (fact, option) in facts.into_iter().zip(options) {
                let key = key_at(&fact, "reference")?;
                if !selected.contains(&key)
                    || key_at(&option, "fact")? != key
                    || !page_keys.insert(key.clone())
                {
                    return Err(InvestigationError::InvalidInput);
                }
                observed.insert(key);
                fact_indexes.push(intern(&mut result.facts, fact));
                option_indexes.push(intern(&mut result.evidence_options, option));
            }
            // Other context metadata is retained verbatim, including any future
            // coverage annotations. No semantic summary is manufactured here.
            context.insert("fact_indexes".into(), json!(fact_indexes));
            let mut compact_query = json!({
                "query_index": index,
                "selection": manifest.query,
                "context": context,
                "evidence_option_indexes": option_indexes,
            });
            if let Some(qualification) = qualification {
                compact_query["qualification"] = qualification;
            }
            result.queries.push(compact_query);
        }
        if observed.iter().collect::<BTreeSet<_>>() != selected {
            return Err(InvestigationError::InvalidInput);
        }
        bounded_bytes(&result, MAX_INPUT_BYTES)?;
        Ok(result)
    }
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

fn intern(inventory: &mut Vec<Value>, value: Value) -> usize {
    if let Some(index) = inventory.iter().position(|prior| prior == &value) {
        index
    } else {
        inventory.push(value);
        inventory.len() - 1
    }
}
