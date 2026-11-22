//! Which pipeline agents have work in flight (#256, AC-1808).
//!
//! A pipeline agent is never dispatched; it runs inside other work. The IaC
//! Grapher runs inside every repository recovery, which the job store
//! records. The Flow Tracer and the Contract Matcher run inside a semantic
//! preview, which is not a job, so each preview holds an activity guard for
//! as long as it runs. Guards are counted per app state and live in this
//! process only: nothing is persisted, and a restart starts idle.
use agents::pipeline_roster::PipelinePass;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Semantic previews in flight, keyed by the app state they run against.
static SEMANTIC_PREVIEWS: Mutex<BTreeMap<usize, usize>> = Mutex::new(BTreeMap::new());

fn key(state: &crate::AppState) -> usize {
    std::ptr::from_ref(state) as usize
}

/// Held for the whole of one semantic preview.
pub(crate) struct SemanticPreviewActivity {
    key: usize,
}

impl SemanticPreviewActivity {
    pub(crate) fn begin(state: &crate::AppState) -> Self {
        let key = key(state);
        if let Ok(mut previews) = SEMANTIC_PREVIEWS.lock() {
            *previews.entry(key).or_default() += 1;
        }
        Self { key }
    }
}

impl Drop for SemanticPreviewActivity {
    fn drop(&mut self) {
        if let Ok(mut previews) = SEMANTIC_PREVIEWS.lock()
            && let Some(count) = previews.get_mut(&self.key)
        {
            *count -= 1;
            if *count == 0 {
                previews.remove(&self.key);
            }
        }
    }
}

fn semantic_previews(state: &crate::AppState) -> usize {
    SEMANTIC_PREVIEWS
        .lock()
        .map(|previews| previews.get(&key(state)).copied().unwrap_or(0))
        .unwrap_or(0)
}

/// The job kind prefixes of a repository recovery, which runs IaC recovery.
/// No prefix is a prefix of another, so a job counts once.
const RECOVERY_KINDS: [&str; 4] = ["ingest-source-v1:", "ingest:", "add-repo:", "add-system:"];

/// How many runs of work that includes `pass` are in flight now.
pub(crate) fn active_by_pass(
    state: &crate::AppState,
) -> Result<impl Fn(PipelinePass) -> usize, String> {
    let recoveries = state
        .jobs
        .lock()
        .map_err(|error| error.to_string())?
        .count_running_with_prefix(&RECOVERY_KINDS)
        .map_err(|error| error.to_string())?;
    let previews = semantic_previews(state);
    Ok(move |pass| match pass {
        PipelinePass::IacGraph => recoveries,
        PipelinePass::FlowTrace | PipelinePass::ContractMatch => previews,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-1808: the IaC Grapher counts running recoveries only, and the Flow
    // Tracer and Contract Matcher count the semantic previews in flight for
    // this app state, back to idle when the preview ends.
    #[test]
    fn pipeline_activity_counts_recoveries_and_previews_in_flight() {
        let app_data = tempfile::tempdir().unwrap();
        let state = crate::registered_source_tests::app_state(app_data.path());
        let active = |pass| active_by_pass(&state).unwrap()(pass);
        assert_eq!(active(PipelinePass::IacGraph), 0);
        assert_eq!(active(PipelinePass::FlowTrace), 0);
        let (running, _execution) = crate::start_job(&state, "ingest-source-v1:src_1").unwrap();
        assert_eq!(running.status, "running");
        let (_, _other) = crate::start_job(&state, "plugin-gate:t0.plugin").unwrap();
        state
            .jobs
            .lock()
            .unwrap()
            .enqueue("add-repo:https://example.test/queued")
            .unwrap();
        assert_eq!(active(PipelinePass::IacGraph), 1);
        assert_eq!(active(PipelinePass::ContractMatch), 0);
        {
            let _first = SemanticPreviewActivity::begin(&state);
            let _second = SemanticPreviewActivity::begin(&state);
            assert_eq!(active(PipelinePass::FlowTrace), 2);
            assert_eq!(active(PipelinePass::ContractMatch), 2);
            assert_eq!(active(PipelinePass::IacGraph), 1);
        }
        assert_eq!(active(PipelinePass::FlowTrace), 0);
        let (_, _near) = crate::start_job(&state, "ingestion").unwrap();
        let (_, _gap) = crate::start_job(&state, "escalate:gap:local").unwrap();
        assert_eq!(active(PipelinePass::IacGraph), 1);
        let (_, _system) = crate::start_job(&state, "add-system:/sys").unwrap();
        assert_eq!(active(PipelinePass::IacGraph), 2);
    }
}
