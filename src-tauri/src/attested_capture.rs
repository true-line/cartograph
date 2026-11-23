//! Capturing attested records in the app (ADR-0090 decision 9, AC-1079).
//!
//! These are the engineer's paths, and they are Tauri commands only. No MCP
//! tool reaches them, so an MCP session can never state, confirm, dispute
//! or withdraw a record.
//!
//! - [`set_engineer`] records the project's one engineer (v1). The name is
//!   kept in the project's contributor table and nowhere else.
//! - [`state_gap_answer`] lets the engineer answer an open Gap from the Gaps
//!   register. The answer is engineer-backed from the start, and the Gap
//!   stays a Gap in the graph and in the count.
//! - [`engineer_attest`] lets the engineer confirm, dispute or withdraw a
//!   stored record at the attestation count they read. Confirming is what
//!   makes an agent's record engineer-backed.
//! - [`attested_records`] lists the stored records, newest first.
//!
//! Recorded hashes always come from the current graph, so a record made
//! here goes stale as soon as a fact it rests on changes.

use agents::attested::{
    Attestation, AttestationSource, AttestedRecord, AttestedTarget, EngineerVerb,
};
use agents::attested_store::{AttestedPage, Contributor, StoredAttested};
use std::collections::BTreeSet;
use tauri::Manager;

/// The engineer's contributor, or the closed reason there is none.
fn engineer(state: &crate::AppState) -> Result<Contributor, String> {
    let drafts = state.drafts.lock().map_err(|e| e.to_string())?;
    drafts
        .attested()
        .engineer()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no_engineer".to_owned())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Record the project's engineer. v1 holds one, so a second call is
/// refused.
pub(crate) fn set_engineer(
    state: &crate::AppState,
    display_name: &str,
    handle: Option<&str>,
) -> Result<Contributor, String> {
    let drafts = state.drafts.lock().map_err(|e| e.to_string())?;
    drafts
        .attested()
        .set_engineer(display_name, handle)
        .map_err(|e| e.to_string())
}

/// The engineer's answer to the Gap `gap_id` for `slot`, resting on the
/// `supporting` citation ids, staged as stated from the Gaps register. A
/// Gap the graph does not hold, or holds as recovered, is refused.
pub(crate) fn state_gap_answer(
    state: &crate::AppState,
    gap_id: &str,
    slot: &str,
    statement: &str,
    supporting: Vec<String>,
    at_ms: i64,
) -> Result<StoredAttested, String> {
    let engineer = engineer(state)?;
    let stated = Attestation::Engineer {
        contributor: engineer.id,
        verb: EngineerVerb::Stated,
        at_ms,
        source: AttestationSource::GapsRegister,
        reference: None,
    };
    let cited: BTreeSet<String> = supporting.iter().cloned().collect();
    let recorded_hashes = {
        let graph = state.graph.lock().map_err(|e| e.to_string())?;
        let (nodes, edges) = graph.read_snapshot().map_err(|e| e.to_string())?;
        let gap = nodes
            .iter()
            .find(|node| node.id == gap_id)
            .ok_or("gap_not_found")?;
        if !context_hub::curated::is_gap(gap) {
            return Err("gap_recovered".into());
        }
        let hashes = spec::cited_fact_hashes(&nodes, &edges, &cited);
        if hashes.len() != cited.len() {
            return Err("supporting_fact_not_found".into());
        }
        hashes
    };
    let record = AttestedRecord {
        target: AttestedTarget::Gap {
            gap_id: gap_id.to_owned(),
            slot: slot.to_owned(),
        },
        statement: statement.to_owned(),
        supporting,
        recorded_hashes,
        attestations: vec![stated.clone()],
    };
    let drafts = state.drafts.lock().map_err(|e| e.to_string())?;
    let store = drafts.attested();
    let stored = store.stage(record).map_err(|e| e.to_string())?;
    // The same content may already be stored, stated by an agent or by the
    // engineer before: `stage` then returns it unchanged, so the engineer's
    // statement is appended unless it is already the engineer's latest.
    let latest = stored
        .record
        .attestations
        .iter()
        .rev()
        .find(|attestation| attestation.contributor() == stated.contributor());
    let already_stated = matches!(
        latest,
        Some(Attestation::Engineer {
            verb: EngineerVerb::Stated | EngineerVerb::Confirmed,
            ..
        })
    );
    if already_stated {
        return Ok(stored);
    }
    store
        .attest(&stored.record_id, stored.attestation_count(), stated)
        .map_err(|e| e.to_string())
}

/// The engineer confirms, disputes or withdraws the stored record
/// `record_id`, read at `expected_count` attestations. Stating is not a
/// review: a statement is a new record.
pub(crate) fn engineer_attest(
    state: &crate::AppState,
    record_id: &str,
    expected_count: u64,
    verb: EngineerVerb,
    source: AttestationSource,
    reference: Option<String>,
    at_ms: i64,
) -> Result<StoredAttested, String> {
    if verb == EngineerVerb::Stated {
        return Err("stated_is_not_a_review".into());
    }
    let engineer = engineer(state)?;
    let drafts = state.drafts.lock().map_err(|e| e.to_string())?;
    drafts
        .attested()
        .attest(
            record_id,
            expected_count,
            Attestation::Engineer {
                contributor: engineer.id,
                verb,
                at_ms,
                source,
                reference,
            },
        )
        .map_err(|e| e.to_string())
}

/// Stored records, newest first.
pub(crate) fn attested_records(
    state: &crate::AppState,
    limit: usize,
    before: Option<u64>,
) -> Result<AttestedPage, String> {
    let drafts = state.drafts.lock().map_err(|e| e.to_string())?;
    drafts
        .attested()
        .list(limit, before)
        .map_err(|e| e.to_string())
}

/// Record the project's engineer.
#[tauri::command]
pub(crate) async fn set_attested_engineer(
    display_name: String,
    handle: Option<String>,
    app: tauri::AppHandle,
) -> Result<Contributor, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<crate::AppState>();
        set_engineer(&state, &display_name, handle.as_deref())
    })
    .await
}

/// The engineer answers a Gap from the Gaps register.
#[tauri::command]
pub(crate) async fn state_attested_gap_answer(
    gap_id: String,
    slot: String,
    statement: String,
    supporting: Vec<String>,
    app: tauri::AppHandle,
) -> Result<StoredAttested, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<crate::AppState>();
        state_gap_answer(&state, &gap_id, &slot, &statement, supporting, now_ms())
    })
    .await
}

/// The engineer confirms, disputes or withdraws an attested record.
#[tauri::command]
pub(crate) async fn review_attested_record(
    record_id: String,
    expected_count: u64,
    verb: EngineerVerb,
    source: AttestationSource,
    reference: Option<String>,
    app: tauri::AppHandle,
) -> Result<StoredAttested, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<crate::AppState>();
        engineer_attest(
            &state,
            &record_id,
            expected_count,
            verb,
            source,
            reference,
            now_ms(),
        )
    })
    .await
}

/// The stored attested records, newest first.
#[tauri::command]
pub(crate) async fn list_attested_records(
    limit: usize,
    before: Option<u64>,
    app: tauri::AppHandle,
) -> Result<AttestedPage, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<crate::AppState>();
        attested_records(&state, limit, before)
    })
    .await
}

#[cfg(test)]
#[path = "attested_capture_tests.rs"]
mod tests;
