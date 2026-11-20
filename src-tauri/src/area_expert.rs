//! The topic expert (#2057, AC-0932): one question about one subsystem
//! area, answered in one bounded call from that area's compiled briefing.
//!
//! 1. [`prepare`] compiles the area's briefing (#2055) from one read of the
//!    graph, the decisions and the knowledge overlay, and writes the one
//!    completion action (`agents::area_expert::action`).
//! 2. [`area_ask_preview`] shows the exact redacted payload. A cloud answer
//!    runs only for the payload hash the user approved, through the same
//!    firewall, policy, consent and egress record as a composition (#1991).
//! 3. [`run_area_ask`] makes the call as a durable job and decodes the
//!    reply: statements keep only citation ids the briefing prints, and an
//!    area to ask instead only when the briefing names it as a neighbour.
//!
//! The answer is returned to the caller with the usage the provider
//! reported and labelled with the role `topic-expert@1`. Nothing here
//! writes the graph (R-INT-1, R-INT-3).

use crate::area_briefing::AreaBriefingView;
use crate::{AppState, JobExecution};
use agents::area_expert::{self, ExpertAnswer, ExpertInput, ExpertTurn, TOPIC_EXPERT_ROLE};
use llm::bounded::CallDirective;
use llm::{CompletionAction, EgressPreview, LlmProvider};
use serde::Serialize;
use std::collections::BTreeSet;
use tauri::Manager;

/// Everything one answer is made from, read once.
pub(crate) struct Prepared {
    view: AreaBriefingView,
    action: CompletionAction,
    citable: BTreeSet<String>,
    neighbours: BTreeSet<String>,
}

impl Prepared {
    /// The one completion action of the question.
    #[cfg(test)]
    pub(crate) fn action(&self) -> &CompletionAction {
        &self.action
    }
}

/// An area a question can be asked of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AreaChoice {
    /// The Subsystem id.
    pub(crate) area: String,
    /// The area's name.
    pub(crate) name: String,
    /// Its member files.
    pub(crate) files: usize,
    /// The D1 sections that hold a block for the area.
    pub(crate) sections: Vec<&'static str>,
    /// The size of its briefing, in bytes.
    pub(crate) briefing_bytes: usize,
}

/// What one call used, as the provider reported it. Not verified billing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AskUsage {
    /// The model the request named.
    pub(crate) model: String,
    /// Input tokens, when the provider reported them.
    pub(crate) input_tokens: Option<u64>,
    /// Output tokens, when the provider reported them.
    pub(crate) output_tokens: Option<u64>,
    /// The size of the request, in bytes.
    pub(crate) request_bytes: u64,
}

/// One call's decoded reply and what it used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Asked {
    /// The answer, or the closed reason there is none.
    pub(crate) answer: Result<ExpertAnswer, &'static str>,
    /// What the call used.
    pub(crate) usage: AskUsage,
}

/// What asking produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum AskOutcome {
    /// An answer.
    Answered {
        /// The Subsystem id the answer speaks for.
        area: String,
        /// Its name.
        area_name: String,
        /// The role that wrote it.
        role: &'static str,
        /// The durable job that produced it.
        job_id: String,
        /// What the call used.
        usage: AskUsage,
        /// The answer.
        #[serde(flatten)]
        answer: ExpertAnswer,
    },
    /// No answer, and the closed reason.
    NoAnswer {
        /// Why.
        reason: &'static str,
        /// What the call used.
        usage: AskUsage,
    },
}

/// The areas of the current overlay, by name. Empty when no overlay is
/// current for the graph.
pub(crate) fn areas(state: &AppState) -> Result<Vec<AreaChoice>, String> {
    let mut areas: Vec<AreaChoice> = match crate::area_briefing::compile(state, None) {
        Ok(views) => views
            .into_iter()
            .map(|view| AreaChoice {
                briefing_bytes: view.text.len(),
                area: view.area,
                name: view.name,
                files: view.files,
                sections: view.sections,
            })
            .collect(),
        Err(reason) if reason == "area_not_found" => Vec::new(),
        Err(error) => return Err(error),
    };
    areas.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.area.cmp(&b.area)));
    Ok(areas)
}

/// Compile the briefing of `area` and write the question's action.
pub(crate) fn prepare(
    state: &AppState,
    area: &str,
    question: &str,
    turns: &[ExpertTurn],
) -> Result<Prepared, String> {
    if !area_expert::question_admissible(question) {
        return Err("question_not_admissible".into());
    }
    let view = crate::area_briefing::compile(state, Some(area))?
        .pop()
        .ok_or("area_not_found")?;
    let citable = spec::briefing_citation_ids(&view.text);
    let neighbours = view.neighbours.iter().cloned().collect();
    let action = area_expert::action(
        &ExpertInput {
            area: &view.name,
            briefing: &view.text,
            turns,
            question,
        },
        format!("area-ask:{}", view.area),
    );
    Ok(Prepared {
        view,
        action,
        citable,
        neighbours,
    })
}

/// Make the call and decode its reply.
pub(crate) fn ask(
    state: &AppState,
    provider: &dyn LlmProvider,
    prepared: &Prepared,
    approved_payload_hash: Option<String>,
    check: &dyn Fn() -> CallDirective,
) -> Result<Asked, String> {
    let completion = crate::section_compose::complete(
        state,
        provider,
        &prepared.action,
        approved_payload_hash,
        check,
    )?;
    let reported = completion.usage.as_ref();
    Ok(Asked {
        answer: area_expert::decode(&completion.text, &prepared.citable, &prepared.neighbours)
            .map_err(|refusal| refusal.code()),
        usage: AskUsage {
            model: completion.requested_model,
            input_tokens: reported.and_then(|usage| usage.input_tokens),
            output_tokens: reported.and_then(|usage| usage.output_tokens),
            request_bytes: completion.request_bytes,
        },
    })
}

/// The areas a question can be asked of.
#[tauri::command]
pub(crate) async fn area_choices(app: tauri::AppHandle) -> Result<Vec<AreaChoice>, String> {
    crate::off_ui_thread(move || areas(&app.state::<AppState>())).await
}

/// The exact redacted payload a cloud answer to `question` would send.
#[tauri::command]
pub(crate) async fn area_ask_preview(
    area: String,
    question: String,
    turns: Vec<ExpertTurn>,
    app: tauri::AppHandle,
) -> Result<EgressPreview, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        let prepared = prepare(&state, &area, &question, &turns)?;
        let provider = crate::egress::escalation_provider("cloud")?;
        let bounded = crate::section_compose::firewall(&state)?
            .prepare_bounded(
                provider.as_ref(),
                &prepared.action,
                &crate::section_compose::limits()?,
            )
            .map_err(|error| error.to_string())?;
        Ok(bounded.preview().egress.clone())
    })
    .await
}

fn run_blocking(
    state: &AppState,
    execution: &JobExecution,
    provider: &dyn LlmProvider,
    (area, question, turns): (&str, &str, &[ExpertTurn]),
    approved_payload_hash: Option<String>,
) -> Result<AskOutcome, String> {
    let prepared = prepare(state, area, question, turns)?;
    let check = || match crate::job_cancelled(state, execution) {
        Ok(false) => CallDirective::Continue,
        Ok(true) => CallDirective::Cancelled,
        Err(_) => CallDirective::OwnershipLost,
    };
    let Asked { answer, usage } = ask(state, provider, &prepared, approved_payload_hash, &check)?;
    Ok(match answer {
        Ok(answer) => AskOutcome::Answered {
            area: prepared.view.area,
            area_name: prepared.view.name,
            role: TOPIC_EXPERT_ROLE,
            job_id: execution.id().to_string(),
            usage,
            answer,
        },
        Err(reason) => AskOutcome::NoAnswer { reason, usage },
    })
}

/// Ask the topic expert of `area` one question, as a durable job. A cloud
/// answer runs only when `approved_payload_hash` is the hash of the payload
/// this exact read produces (one-action consent, AC-0063).
#[tauri::command]
pub(crate) async fn run_area_ask(
    area: String,
    question: String,
    turns: Vec<ExpertTurn>,
    mode: String,
    approved_payload_hash: Option<String>,
    app: tauri::AppHandle,
) -> Result<AskOutcome, String> {
    let state = app.state::<AppState>();
    let (running, execution) = crate::start_job(&state, &format!("area-ask:{mode}"))?;
    crate::emit_job(&app, &running);
    let worker = app.clone();
    let result = crate::off_ui_thread_for_job(execution.clone(), move |execution| {
        let state = worker.state::<AppState>();
        let provider = crate::egress::escalation_provider(&mode)?;
        run_blocking(
            &state,
            execution,
            provider.as_ref(),
            (&area, &question, &turns),
            approved_payload_hash,
        )
    })
    .await;
    match &result {
        Ok(_) => {
            let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
            let update = jobs
                .finish_execution(&execution, &[])
                .map_err(|e| e.to_string())?;
            drop(jobs);
            crate::emit_job(&app, &crate::updated_job(update));
        }
        Err(error) => crate::report_failure(&app, &state, &execution, error),
    }
    result
}

#[cfg(test)]
#[path = "area_expert_tests.rs"]
mod tests;
