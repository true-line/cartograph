//! Child-only presentation; never a replacement durable result or authority.
use super::*;
use core_prov::content_hash;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const SCHEMA: &str = "saved-parent-history@2";
const IDENTITY: &str = "Withheld findings are absent; saved ids are final.";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WithholdingSummary {
    disposition: WithholdingDisposition,
    withheld_count: usize,
    saved_finding_count: usize,
    identity: String,
}

/// Called only after JobStore authenticates the complete original parent. The
/// existing bounded canonical envelope is reused, avoiding an unbounded clone.
/// The summary is a sibling of saved_result: its result_id still identifies the
/// original durable result, not this explicitly versioned prompt projection.
pub fn project_saved_parent_history(
    value: &mut Value,
    limit: usize,
) -> Result<(), InvestigationError> {
    // Bind the authenticated durable result before changing its presentation.
    let source_result_hash = if value["saved_result"].is_null() {
        None
    } else {
        let result: InvestigationResult = serde_json::from_value(value["saved_result"].clone())
            .map_err(|_| InvestigationError::InvalidInput)?;
        Some(content_hash(&bounded_bytes(&result, limit)?))
    };
    let saved = &mut value["saved_result"];
    let count = saved["findings"].as_array().map_or(0, Vec::len);
    let witness = saved
        .get_mut("completion")
        .and_then(Value::as_object_mut)
        .and_then(|c| c.remove("withholding"));
    let summary = if let Some(witness) = witness {
        let witness: FindingWithholdingWitness =
            serde_json::from_value(witness).map_err(|_| InvestigationError::InvalidInput)?;
        witness.validate_shape()?;
        if witness.survivor_indexes.len() != count {
            return Err(InvestigationError::InvalidInput);
        }
        Some(WithholdingSummary {
            disposition: witness.disposition,
            withheld_count: witness.withheld_indexes.len(),
            saved_finding_count: count,
            identity: IDENTITY.into(),
        })
    } else {
        None
    };
    let object = value
        .as_object_mut()
        .ok_or(InvestigationError::InvalidInput)?;
    object.insert(
        "source_result_hash".into(),
        serde_json::to_value(source_result_hash).map_err(|_| InvestigationError::InvalidInput)?,
    );
    object.insert("projection_schema".into(), Value::String(SCHEMA.into()));
    object.insert(
        "withholding_summary".into(),
        serde_json::to_value(summary).map_err(|_| InvestigationError::InvalidInput)?,
    );
    let hash = content_hash(&bounded_bytes(value, limit)?);
    value["projection_hash"] = Value::String(hash);
    bounded_bytes(value, limit)?;
    Ok(())
}

/// Checks the projection's own binding before the existing parent-prose privacy
/// reader consumes it. The ledger separately binds the entire supplied history.
/// Removing projection-only fields here is local parsing, never a durable write.
pub(super) fn verify_saved_parent_projection(
    value: &mut Value,
    limit: usize,
) -> Result<(), InvestigationError> {
    let object = value
        .as_object_mut()
        .ok_or(InvestigationError::InvalidInput)?;
    let hash = object
        .remove("projection_hash")
        .ok_or(InvestigationError::InvalidInput)?;
    if hash.as_str() != Some(content_hash(&bounded_bytes(value, limit)?).as_str()) {
        return Err(InvestigationError::InvalidInput);
    }
    let object = value
        .as_object_mut()
        .ok_or(InvestigationError::InvalidInput)?;
    if object
        .remove("projection_schema")
        .as_ref()
        .and_then(Value::as_str)
        != Some(SCHEMA)
    {
        return Err(InvestigationError::InvalidInput);
    }
    let source_hash = object
        .remove("source_result_hash")
        .ok_or(InvestigationError::InvalidInput)?;
    let absent = object.get("saved_result").is_none_or(Value::is_null);
    if if absent {
        !source_hash.is_null()
    } else {
        !source_hash
            .as_str()
            .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    } {
        return Err(InvestigationError::InvalidInput);
    }
    let summary = object
        .remove("withholding_summary")
        .ok_or(InvestigationError::InvalidInput)?;
    let summary: Option<WithholdingSummary> =
        serde_json::from_value(summary).map_err(|_| InvestigationError::InvalidInput)?;
    let saved = &value["saved_result"];
    if saved["completion"].get("withholding").is_some() {
        return Err(InvestigationError::InvalidInput);
    }
    if let Some(summary) = summary {
        if summary.identity != IDENTITY
            || summary.withheld_count == 0
            || saved["findings"].as_array().map(Vec::len) != Some(summary.saved_finding_count)
            || saved["completion"]["outcome"] != "findings_withheld_after_correction"
        {
            return Err(InvestigationError::InvalidInput);
        }
    } else if saved["completion"]["outcome"] == "findings_withheld_after_correction" {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auditor15_projection_binding_refuses_tampering_and_legacy_confusion() {
        // AC-0600: absent results remain absent, and a hash is not authority to insert positions.
        let mut value = serde_json::json!({"saved_result":null});
        project_saved_parent_history(&mut value, 32768).unwrap();
        assert!(verify_saved_parent_projection(&mut value.clone(), 32768).is_ok());
        let mut changed = value.clone();
        changed["projection_schema"] = "saved-parent-history@1".into();
        assert!(verify_saved_parent_projection(&mut changed, 32768).is_err());
        changed = value.clone();
        changed["saved_result"] =
            serde_json::json!({"completion":{"withholding":{"withheld_indexes":[2]}}});
        changed.as_object_mut().unwrap().remove("projection_hash");
        changed["projection_hash"] = content_hash(&bounded_bytes(&changed, 32768).unwrap()).into();
        assert!(verify_saved_parent_projection(&mut changed, 32768).is_err());
        value.as_object_mut().unwrap().remove("projection_hash");
        assert!(verify_saved_parent_projection(&mut value, 32768).is_err());
    }
}
