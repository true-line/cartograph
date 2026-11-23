//! AC-1079 (T-1079, ADR-0090 decision 9): an MCP session stages a finding of
//! its own completed task as its agent's attested answer to a Gap, for
//! review in the app. It can never confirm.
use super::*;
use crate::investigations::mcp_dispatch::TaskDispatcher;
use crate::mcp_reads::{ReadError, task_reservation::TaskReservations};
use agents::attested::{AgentVerb, Attestation, Standing};
use core_graph::GraphStore;
use mcp_local::work_budget::{WorkBudget, WorkLimits};
use mcp_wire::tools::TaskTool;

#[test]
fn an_mcp_task_stages_its_finding_as_an_agent_attested_unconfirmed_gap_answer() {
    let provider = Arc::new(PublicProvider::new(false));
    worker::with_public_start_fixture(provider.clone(), false, |app, mut request, _| {
        request.specialist_id = "domain-analyst@10".into();
        request = request
            .normalized_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap();
        let state = app.state::<AppState>();
        state
            .investigations
            .set_test_provider(request.provider_mode, provider.clone());
        let one = std::num::NonZeroUsize::new(1).unwrap();
        let (client, foreign) = {
            let mut jobs = state.jobs.lock().unwrap();
            let capacity = std::num::NonZeroUsize::new(2).unwrap();
            (
                jobs.create_mcp_client(capacity).unwrap(),
                jobs.create_mcp_client(capacity).unwrap(),
            )
        };
        let grant = McpTaskActivation::granted(client, one);
        let foreign_grant = McpTaskActivation::granted(foreign, one);
        let dispatch = |session| {
            let budget = WorkBudget::new(WorkLimits {
                max_workers: one,
                max_reserved_bytes: one,
                max_cumulative_read_bytes: 0,
            });
            TaskDispatcher::new(
                session,
                TaskReservations::from(Arc::new(budget.try_acquire(one, 0).unwrap())),
            )
        };
        let dispatcher = dispatch(grant.session());
        let other = dispatch(foreign_grant.session());
        let call = |dispatcher: &TaskDispatcher, tool, args: Value| {
            dispatcher.call(
                app,
                &json!("request"),
                tool,
                args.as_object().unwrap().clone(),
                crate::mcp_reads::worker::tests::task_read_limits(),
                256 * 1024,
            )
        };
        let started = call(&dispatcher, TaskTool::Start, json!({"request":request})).unwrap();
        let id = started["result"]["structuredContent"]["investigation_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            finished(app, &id).summary.status,
            InvestigationStatus::Completed
        );
        // A node of the fixture without valid provenance is an open Gap.
        let gap = {
            let graph = state.graph.lock().unwrap();
            let (nodes, _) = graph.read_snapshot().unwrap();
            let gaps: Vec<_> = nodes
                .iter()
                .filter(|node| context_hub::curated::is_gap(node))
                .map(|node| node.id.clone())
                .collect();
            gaps.first().cloned().expect("the fixture holds a Gap")
        };
        let finding = state
            .jobs
            .lock()
            .unwrap()
            .investigation_result(&id)
            .unwrap()
            .unwrap()
            .findings[0]
            .finding_id
            .clone();
        let propose = |dispatcher: &TaskDispatcher, finding: &str, gap: &str| {
            call(
                dispatcher,
                TaskTool::ProposeAttested,
                json!({"task_id":id,"finding_id":finding,"gap_id":gap,"slot":"owner"}),
            )
        };
        // Another session's task reads as unknown, and nothing is staged.
        assert_eq!(
            propose(&other, &finding, &gap),
            Err(ReadError::InvalidRequest)
        );
        assert_eq!(
            propose(&dispatcher, "unknown", &gap),
            Err(ReadError::InvalidRequest)
        );
        assert_eq!(
            propose(&dispatcher, &finding, "gap:unknown"),
            Err(ReadError::InvalidRequest)
        );
        let list = |state: &AppState| {
            let drafts = state.drafts.lock().unwrap();
            drafts.attested().list(10, None).unwrap().items
        };
        assert!(list(&state).is_empty());

        let staged = propose(&dispatcher, &finding, &gap).unwrap();
        let reply = &staged["result"]["structuredContent"];
        assert_eq!(reply["standing"], "agent_attested_unconfirmed");
        assert_eq!(reply["attestation_count"], 1);
        assert!(reply.get("record").is_none(), "no contributors on the wire");
        let stored = list(&state);
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].record_id, reply["record_id"]);
        assert_eq!(stored[0].standing, Standing::AgentAttestedUnconfirmed);
        assert!(!stored[0].record.supporting.is_empty());
        let Attestation::Agent {
            verb,
            run,
            provider: provider_id,
            model,
            directed_by,
            ..
        } = &stored[0].record.attestations[0]
        else {
            panic!("an agent states it")
        };
        assert_eq!(*verb, AgentVerb::Stated);
        assert_eq!(run, &id);
        assert_eq!(provider_id, "public-entry-scripted");
        assert_eq!(model, "public-scripted-model");
        assert_eq!(*directed_by, None, "no engineer directs an MCP proposal");
        // The same proposal again appends nothing.
        let again = propose(&dispatcher, &finding, &gap).unwrap();
        assert_eq!(again["result"]["structuredContent"]["attestation_count"], 1);

        // Once the graph moves, the run's facts are not the current ones.
        state
            .graph
            .lock()
            .unwrap()
            .put_node(&core_graph::Node {
                id: "gap:fixture-other".into(),
                label: "Gap".into(),
                props: json!({"missing": "other"}),
            })
            .unwrap();
        assert_eq!(
            propose(&dispatcher, &finding, &gap),
            Err(ReadError::InvalidRequest)
        );
        assert_eq!(
            crate::attested_capture::propose_from_finding(
                &state, &id, &finding, &gap, "owner", false
            )
            .unwrap_err(),
            "graph_changed"
        );
    });
}
