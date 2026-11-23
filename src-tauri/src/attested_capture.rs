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
//! - [`attested_views`] lists the stored records, newest first, as the
//!   Gaps register and the Spec Workbench show them: label and
//!   contributors by name (AC-1081).
//! - [`propose_from_finding`] takes a finding of a completed investigation
//!   or chat as its agent's answer to a Gap: `agent-attested, unconfirmed`
//!   until the engineer confirms it. The same content from another agent
//!   is recorded as that agent agreeing.
//!
//! Recorded hashes always come from the current graph, so a record made
//! here goes stale as soon as a fact it rests on changes.

use agents::attested::Standing;
use agents::attested::{
    Attestation, AttestationSource, AttestedRecord, AttestedTarget, EngineerVerb,
};
use agents::attested_store::{Contributor, ContributorKind, StoredAttested};
use serde::Serialize;
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

/// One contributor's attestation as the app shows it. The name comes from
/// the project's contributor table at read time and never leaves the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AttestedContributorView {
    pub(crate) display_name: String,
    pub(crate) kind: ContributorKind,
    /// The attestation's verb, such as `stated` or `confirmed`.
    pub(crate) verb: String,
    /// The producing run, for an agent.
    pub(crate) run: Option<String>,
}

/// A stored record as the Gaps register and the Spec Workbench show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AttestedView {
    pub(crate) record_id: String,
    pub(crate) citation_id: String,
    pub(crate) target: AttestedTarget,
    pub(crate) statement: String,
    pub(crate) standing: Standing,
    /// The label every surface prints: `attested`, `agent-attested,
    /// unconfirmed`, `disputed` or `withdrawn`.
    pub(crate) label: &'static str,
    /// The count a review is made at.
    pub(crate) attestation_count: u64,
    /// Every attestation, oldest first.
    pub(crate) contributors: Vec<AttestedContributorView>,
}

/// One page of [`AttestedView`]s, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AttestedViewPage {
    pub(crate) items: Vec<AttestedView>,
    pub(crate) next_before: Option<u64>,
}

/// The stored records with their labels and contributors by name.
pub(crate) fn attested_views(
    state: &crate::AppState,
    limit: usize,
    before: Option<u64>,
) -> Result<AttestedViewPage, String> {
    let drafts = state.drafts.lock().map_err(|e| e.to_string())?;
    let store = drafts.attested();
    let page = store.list(limit, before).map_err(|e| e.to_string())?;
    let mut items = Vec::with_capacity(page.items.len());
    for stored in page.items {
        let mut contributors = Vec::with_capacity(stored.record.attestations.len());
        for attestation in &stored.record.attestations {
            let contributor = store
                .contributor(attestation.contributor())
                .map_err(|e| e.to_string())?
                .ok_or("unknown_contributor")?;
            let (verb, run) = match attestation {
                Attestation::Agent { verb, run, .. } => (word(verb)?, Some(run.clone())),
                Attestation::Engineer { verb, .. } => (word(verb)?, None),
            };
            contributors.push(AttestedContributorView {
                display_name: contributor.display_name,
                kind: contributor.kind,
                verb,
                run,
            });
        }
        items.push(AttestedView {
            attestation_count: stored.attestation_count(),
            record_id: stored.record_id,
            citation_id: stored.citation_id,
            target: stored.record.target,
            statement: stored.record.statement,
            standing: stored.standing,
            label: stored.standing.label(),
            contributors,
        });
    }
    Ok(AttestedViewPage {
        items,
        next_before: page.next_before,
    })
}

/// A verb's wire word.
fn word(verb: &impl Serialize) -> Result<String, String> {
    serde_json::to_value(verb)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| "unknown_verb".to_owned())
}

/// The finding `finding_id` of the completed investigation
/// `investigation_id`, proposed by the run's agent as the answer to the Gap
/// `gap_id` for `slot`. The engineer who chose it, when `directed`, is
/// recorded as directing the agent. The graph must still be the one the
/// run read, so the recorded hashes are the ones the finding rests on.
pub(crate) fn propose_from_finding(
    state: &crate::AppState,
    investigation_id: &str,
    finding_id: &str,
    gap_id: &str,
    slot: &str,
    directed: bool,
) -> Result<StoredAttested, String> {
    use agents::attested_proposal::{ProposalRun, Proposer, follow_up, proposal};
    use agents::investigation::InvestigationStatus;
    let (detail, result, ledger) = {
        let jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let detail = jobs
            .investigation(investigation_id)
            .map_err(|e| e.to_string())?;
        let result = jobs
            .investigation_result(investigation_id)
            .map_err(|e| e.to_string())?;
        let ledger = jobs
            .investigation_ledger(investigation_id)
            .map_err(|e| e.to_string())?;
        (detail, result, ledger)
    };
    if detail.summary.status != InvestigationStatus::Completed {
        return Err("run_not_completed".into());
    }
    let (Some(result), Some(ledger)) = (result, ledger) else {
        return Err("run_not_completed".into());
    };
    let finding = result
        .findings
        .iter()
        .find(|finding| finding.finding_id == finding_id)
        .ok_or("unknown_finding")?;
    let directed_by = if directed {
        Some(engineer(state)?.id)
    } else {
        None
    };
    let (nodes, edges) = {
        let graph = state.graph.lock().map_err(|e| e.to_string())?;
        let (_, ((nodes, edges), _)) = crate::curated::read_graph(&graph)?;
        (nodes, edges)
    };
    let current = context_hub::ContextSnapshot::new(nodes.clone(), edges.clone())
        .ok()
        .map(|snapshot| snapshot.id().to_owned());
    if current.as_deref() != Some(ledger.graph_snapshot_id.as_str()) {
        return Err("graph_changed".into());
    }
    let gap = nodes
        .iter()
        .find(|node| node.id == gap_id)
        .ok_or("gap_not_found")?;
    if !context_hub::curated::is_gap(gap) {
        return Err("gap_recovered".into());
    }
    let run = ProposalRun {
        run: detail.summary.investigation_id.clone(),
        role: serde_json::to_value(detail.summary.specialist_id)
            .ok()
            .and_then(|role| role.as_str().map(str::to_owned))
            .ok_or("unknown_role")?,
        definition_fingerprint: detail.specialist.prompt_fingerprint.clone(),
        provider: detail.provider.provider_id.clone(),
        model: detail.provider.model.clone(),
    };
    let drafts = state.drafts.lock().map_err(|e| e.to_string())?;
    let store = drafts.attested();
    let agent = store
        .agent(&run.role, &run.provider, &run.model)
        .map_err(|e| e.to_string())?;
    let proposer = Proposer {
        agent: agent.id,
        directed_by,
    };
    let record = proposal(
        &run,
        finding,
        &ledger.citations,
        AttestedTarget::Gap {
            gap_id: gap_id.to_owned(),
            slot: slot.to_owned(),
        },
        &proposer,
        |fact| spec::citation_id(&crate::investigations::section_drafts::fact_key(fact)),
        |cited| spec::cited_fact_hashes(&nodes, &edges, cited),
    )
    .map_err(|refusal| refusal.code().to_owned())?;
    if record.recorded_hashes.len() != record.supporting.len() {
        return Err("supporting_fact_not_found".into());
    }
    let stored = store.stage(record).map_err(|e| e.to_string())?;
    match follow_up(&stored, &run, &proposer) {
        None => Ok(stored),
        Some(agreed) => store
            .attest(&stored.record_id, stored.attestation_count(), agreed)
            .map_err(|e| e.to_string()),
    }
}

/// The engineer proposes a finding of a completed run as its agent's answer
/// to a Gap.
#[tauri::command]
pub(crate) async fn propose_attested_from_finding(
    investigation_id: String,
    finding_id: String,
    gap_id: String,
    slot: String,
    app: tauri::AppHandle,
) -> Result<StoredAttested, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<crate::AppState>();
        propose_from_finding(&state, &investigation_id, &finding_id, &gap_id, &slot, true)
    })
    .await
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

/// The stored attested records, newest first, with their labels and
/// contributors by name.
#[tauri::command]
pub(crate) async fn list_attested_records(
    limit: usize,
    before: Option<u64>,
    app: tauri::AppHandle,
) -> Result<AttestedViewPage, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<crate::AppState>();
        attested_views(&state, limit, before)
    })
    .await
}

#[cfg(test)]
#[path = "attested_capture_tests.rs"]
mod tests;
