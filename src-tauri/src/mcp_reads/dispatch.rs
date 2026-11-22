//! Closed MCP adapter over one host-supplied frozen read session. The host must
//! authenticate and frame-limit input before calling this adapter; it grants no
//! activation, store mutation or provider access. Evidence is advertised only
//! with a concrete AppState read backend and explicit retained-read work caps.
//! Clients must consume `structuredContent`; text content is a concise summary,
//! not a second serialization of potentially large context or artifact pages.

use super::{
    ReadError, ReadSession,
    artifacts::{ArtifactCompilation, ArtifactLimit},
};
use context_hub::coverage::QualifiedRequest;
use context_hub::{
    DiscoveryCursor, DiscoveryQuery, DiscoveryRequest, FactKind, QueryCursor, QueryRequest,
    QueryScope,
};
use mcp_wire::admission::{Action, Admission, ReadTool};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    io::{self, Write},
    num::NonZeroUsize,
};

pub(super) const IMPLEMENTED: [ReadTool; 5] = [
    ReadTool::Capabilities,
    ReadTool::Discover,
    ReadTool::Artifacts,
    ReadTool::ReadArtifact,
    ReadTool::Query,
];

mod evidence;
pub(crate) use evidence::EvidenceWorkLimits;

/// Host policy, all supplied explicitly; none of these limits authorize egress.
#[derive(Clone, Copy)]
pub(crate) struct DispatchLimits {
    pub max_response_bytes: NonZeroUsize,
    pub max_request_ids: NonZeroUsize,
    pub trace: flowtracer::TraceLimits,
    pub compile: spec::SelectedCompileLimits,
}

#[derive(Debug, PartialEq)]
pub(crate) enum DispatchAction {
    Respond(Value),
    Ignore,
    Close,
}

pub(crate) struct Dispatcher<'state> {
    #[cfg(test)]
    admission: Admission,
    session: ReadSession,
    session_id: String,
    basis_id: String,
    limits: DispatchLimits,
    evidence: Option<evidence::Backend<'state>>,
    #[cfg(test)]
    closed: bool,
}

impl<'state> Dispatcher<'state> {
    pub(crate) fn new(
        session: ReadSession,
        session_id: String,
        limits: DispatchLimits,
    ) -> Result<Self, ReadError> {
        if session_id.is_empty() {
            return Err(ReadError::InvalidRequest);
        }
        let basis_id = session.basis_id()?;
        Ok(Self {
            #[cfg(test)]
            admission: Admission::new(IMPLEMENTED, limits.max_request_ids),
            session,
            session_id,
            basis_id,
            limits,
            evidence: None,
            #[cfg(test)]
            closed: false,
        })
    }

    /// Attach only the concrete app's read-only retained-evidence adapter.
    pub(crate) fn with_evidence(
        session: ReadSession,
        session_id: String,
        limits: DispatchLimits,
        state: &'state crate::AppState,
        evidence_limits: EvidenceWorkLimits,
    ) -> Result<Self, ReadError> {
        let mut dispatch = Self::new(session, session_id, limits)?;
        dispatch.evidence = Some(evidence::Backend {
            state,
            limits: evidence_limits,
        });
        #[cfg(test)]
        {
            dispatch.admission = Admission::new(dispatch.implemented(), limits.max_request_ids);
        }
        Ok(dispatch)
    }

    fn implemented(&self) -> Vec<ReadTool> {
        let mut tools = IMPLEMENTED.to_vec();
        if self.evidence.is_some() {
            tools.push(ReadTool::Evidence);
        }
        tools
    }

    #[cfg(test)]
    pub(crate) fn receive(&mut self, message: Value) -> DispatchAction {
        if self.closed {
            return DispatchAction::Close;
        }
        let action=match self.admission.receive(message) {
            Action::Ignore=>DispatchAction::Ignore,
            Action::Close(_)=>DispatchAction::Close,
            Action::Respond(response)=>self.bounded(response,self.limits.max_response_bytes.get()),
            Action::ListTools {id,tools}=>self.bounded(json!({"jsonrpc":"2.0","id":id,"result":{"tools":tools.into_iter().map(tool_descriptor).collect::<Vec<_>>()}}),self.limits.max_response_bytes.get()),
            Action::Call {id,tool,arguments}=>self.call_admitted(&id,tool,arguments),
        };
        if action == DispatchAction::Close {
            self.closed = true;
        }
        action
    }

    pub(super) fn call_admitted(
        &self,
        id: &Value,
        tool: ReadTool,
        arguments: Map<String, Value>,
    ) -> DispatchAction {
        let requested = match tool {
            ReadTool::Discover | ReadTool::Query => arguments
                .get("request")
                .and_then(|qualified| qualified.get("request"))
                .and_then(|request| request.get("max_bytes")),
            ReadTool::ReadArtifact | ReadTool::Evidence => arguments.get("max_bytes"),
            _ => None,
        };
        let cap = requested
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > 0)
            .unwrap_or(self.limits.max_response_bytes.get())
            .min(self.limits.max_response_bytes.get());
        let response = self.call(id, tool, arguments).unwrap_or_else(|error| {
            if error == ReadError::InvalidRequest {
                error_response(Some(id), error)
            } else {
                tool_error(id, error)
            }
        });
        self.bounded(response, cap)
    }

    fn bounded(&self, response: Value, cap: usize) -> DispatchAction {
        if fits(&response, cap) {
            return DispatchAction::Respond(response);
        }
        let error = error_response(response.get("id"), ReadError::Limit);
        if fits(&error, cap) {
            DispatchAction::Respond(error)
        } else {
            DispatchAction::Close
        }
    }

    fn check_session(&self, session_id: &str, basis_id: &str) -> Result<(), ReadError> {
        if session_id != self.session_id || basis_id != self.basis_id {
            Err(ReadError::StaleBasis)
        } else {
            Ok(())
        }
    }

    fn call(
        &self,
        id: &Value,
        tool: ReadTool,
        arguments: Map<String, Value>,
    ) -> Result<Value, ReadError> {
        match tool {
            ReadTool::ReadFact => Err(ReadError::InvalidRequest),
            ReadTool::Capabilities => {
                let _: EmptyArgs = parse(arguments)?;
                Ok(success(id, "Context capabilities.", self.capabilities()))
            }
            ReadTool::Artifacts => {
                let args: SessionArgs = parse(arguments)?;
                self.check_session(&args.session_id, &args.basis_id)?;
                Ok(success(
                    id,
                    "Official artifact catalog.",
                    json!({"session_id":self.session_id,"basis_id":self.basis_id,"artifacts":self.session.artifact_catalog()}),
                ))
            }
            ReadTool::Discover => {
                let args: DiscoverArgs = parse(arguments)?;
                self.check_session(&args.session_id, &args.basis_id)?;
                let mut request: QualifiedRequest<DiscoveryRequest> = args.request.into();
                // Reject invalid original caps before subtraction could hide one.
                if request.request.max_bytes == 0
                    || request.request.max_bytes > context_hub::MAX_RESPONSE_BYTES
                {
                    return Err(ReadError::InvalidRequest);
                }
                let total_cap = request
                    .request
                    .max_bytes
                    .min(self.limits.max_response_bytes.get());
                let template = success(
                    id,
                    "Qualified discovery page.",
                    json!({"session_id":self.session_id,"basis_id":self.basis_id,"context":Value::Null}),
                );
                request.request.max_bytes = inner_cap(&template, total_cap)?;
                let response = self.session.discover(request)?;
                Ok(success(
                    id,
                    "Qualified discovery page.",
                    json!({"session_id":self.session_id,"basis_id":self.basis_id,"context":response}),
                ))
            }
            ReadTool::ReadArtifact => {
                let args: ArtifactArgs = parse(arguments)?;
                self.check_session(&args.session_id, &args.basis_id)?;
                if args.offset > 0 && args.expected_content_id.is_none() {
                    return Err(ReadError::InvalidRequest);
                }
                let cap = args
                    .max_bytes
                    .get()
                    .min(self.limits.max_response_bytes.get());
                let template = success(
                    id,
                    "Official artifact content page.",
                    json!({"session_id":self.session_id,"artifact":Value::Null}),
                );
                let page_cap =
                    NonZeroUsize::new(inner_cap(&template, cap)?).ok_or(ReadError::Limit)?;
                match self.session.compile_artifact(
                    &args.artifact_id,
                    self.limits.trace,
                    self.limits.compile,
                )? {
                    ArtifactCompilation::Produced(artifact) => {
                        let expected = args
                            .expected_content_id
                            .as_deref()
                            .unwrap_or(artifact.content_id());
                        let page = artifact.page(
                            &args.basis_id,
                            expected,
                            args.offset,
                            args.max_content_bytes,
                            page_cap,
                        )?;
                        Ok(success(
                            id,
                            "Official artifact content page.",
                            json!({"session_id":self.session_id,"artifact":page}),
                        ))
                    }
                    ArtifactCompilation::Limited { artifact_id, limit } => {
                        let mut response = success(
                            id,
                            "Official artifact limited.",
                            json!({"status":"artifact_limited","session_id":self.session_id,"basis_id":self.basis_id,"artifact_id":artifact_id,"limit":limit_value(limit)}),
                        );
                        response["result"]["isError"] = json!(true);
                        if !fits(&response, cap) {
                            return Err(ReadError::Limit);
                        }
                        Ok(response)
                    }
                }
            }
            ReadTool::Evidence => self.call_evidence(id, arguments),
            ReadTool::Query => {
                let args: QueryArgs = parse(arguments)?;
                self.check_session(&args.session_id, &args.basis_id)?;
                let mut request: QualifiedRequest<QueryRequest> = args.request.into();
                // Reject invalid original caps before subtraction could hide one.
                if request.request.max_bytes == 0
                    || request.request.max_bytes > context_hub::MAX_RESPONSE_BYTES
                {
                    return Err(ReadError::InvalidRequest);
                }
                let total_cap = request
                    .request
                    .max_bytes
                    .min(self.limits.max_response_bytes.get());
                let template = success(
                    id,
                    "Qualified fact page.",
                    json!({"session_id":self.session_id,"basis_id":self.basis_id,"context":Value::Null}),
                );
                request.request.max_bytes = inner_cap(&template, total_cap)?;
                let response = self.session.query(request)?;
                Ok(success(
                    id,
                    "Qualified fact page.",
                    json!({"session_id":self.session_id,"basis_id":self.basis_id,"context":response}),
                ))
            }
        }
    }

    fn capabilities(&self) -> Value {
        capabilities_value(
            &self.session_id,
            &self.basis_id,
            self.session.basis(),
            &self.implemented(),
            self.session.graph_limits,
            self.limits,
            self.evidence.as_ref().map(|b| b.limits),
        )
    }
}

fn capabilities_value(
    session_id: &str,
    basis_id: &str,
    basis: &super::ReadBasis,
    tools: &[ReadTool],
    graph: core_graph::SnapshotReadLimits,
    limits: DispatchLimits,
    evidence: Option<EvidenceWorkLimits>,
) -> Value {
    json!({"session_id":session_id,"basis_id":basis_id,"basis":basis,"implemented_tools":tools.iter().copied().map(ReadTool::name).collect::<Vec<_>>(),"worker_reservation_model":super::worker::WORKER_MODEL,"structured_content_required":true,"h3_curated_available":basis.h3_curated_available,"curated_access":{"via":"official_artifact","artifact_id":super::curated_read::ARTIFACT_ID,"structured_artifact_id":super::curated_read::JSON_ARTIFACT_ID,"revision":"basis.curated"},"evidence_available":evidence.is_some(),"evidence_absence_reason_available":false,"forgotten_receipt_classification":{"available":evidence.is_some(),"scope":"exact_current_basis_binding_receipt_id","partial":true,"omitted_available":false,"unknown_is_forgotten":false},"receipt_omission_classification":{"available":evidence.is_some(),"scope":"exact_current_fact_same_transaction_omission","partial":true,"kinds":["declarations","rules","store_capacity"],"unknown_is_omitted":false,"not_omitted_is_receipt_available":false},"context_fact_kinds":["node","edge"],"discovery_fact_kinds":["node"],"relations_access":{"via":ReadTool::Query.name(),"query_tool_available":tools.contains(&ReadTool::Query),"scopes":["all","node","neighborhood"],"max_neighborhood_hops":context_hub::MAX_NEIGHBORHOOD_HOPS,"max_facts":context_hub::MAX_FACTS},"flows_access":{"via":"official_artifact","artifact_id":"flow-dossiers"},"limits":{"graph":{"scope":"complete_recovered_graph","max_rows":graph.max_rows,"hard_max_rows":core_graph::SnapshotReadLimits::MAX_ROWS,"max_bytes":graph.max_bytes,"max_depth":graph.max_depth,"max_values":graph.max_values},"max_response_bytes":limits.max_response_bytes.get(),"max_request_ids":limits.max_request_ids.get(),"evidence":evidence.map(|e|json!({"max_file_validation_bytes":e.max_file_validation_bytes.get(),"max_span_bytes":e.max_span_bytes.get()})),"trace":{"max_flows":limits.trace.max_flows,"max_hops":limits.trace.max_hops,"max_output_bytes":limits.trace.max_output_bytes},"selected_compile":{"max_bytes":limits.compile.max_bytes,"adr":{"max_rows":limits.compile.adr.max_rows,"max_output_bytes":limits.compile.adr.max_output_bytes}}}})
}
mod logical;
pub(crate) use logical::logical_protocol_workspace_bytes;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionArgs {
    session_id: String,
    basis_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscoverArgs {
    session_id: String,
    basis_id: String,
    request: QualifiedDiscovery,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QualifiedDiscovery {
    request: StrictDiscovery,
    #[serde(default)]
    context_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictDiscovery {
    query: StrictQuery,
    #[serde(default)]
    labels: Vec<String>,
    max_candidates: usize,
    max_bytes: usize,
    #[serde(default)]
    cursor: Option<StrictCursor>,
}
#[derive(Deserialize)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
enum StrictQuery {
    Identifier { id: String },
    Name { name: String },
    Text { text: String },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictCursor {
    snapshot_id: String,
    policy: String,
    query_id: String,
    offset: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactArgs {
    session_id: String,
    basis_id: String,
    artifact_id: String,
    #[serde(default)]
    expected_content_id: Option<String>,
    offset: usize,
    max_content_bytes: NonZeroUsize,
    max_bytes: NonZeroUsize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryArgs {
    session_id: String,
    basis_id: String,
    request: QualifiedQuery,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QualifiedQuery {
    request: StrictQueryRequest,
    #[serde(default)]
    context_id: Option<String>,
}
/// [`QueryRequest`] with every budget required and no unknown field, so a
/// typo is refused rather than read as "select everything" (AC-1809).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictQueryRequest {
    scope: StrictScope,
    #[serde(default)]
    kind: Option<FactKind>,
    #[serde(default)]
    labels: Vec<String>,
    max_facts: usize,
    max_bytes: usize,
    #[serde(default)]
    cursor: Option<StrictQueryCursor>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum StrictScope {
    All,
    Node { id: String },
    Neighborhood { anchor: String, hops: usize },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictQueryCursor {
    snapshot_id: String,
    selection_id: String,
    offset: usize,
}
impl From<QualifiedQuery> for QualifiedRequest<QueryRequest> {
    fn from(value: QualifiedQuery) -> Self {
        let request = value.request;
        Self {
            context_id: value.context_id,
            request: QueryRequest {
                scope: match request.scope {
                    StrictScope::All => QueryScope::All,
                    StrictScope::Node { id } => QueryScope::Node { id },
                    StrictScope::Neighborhood { anchor, hops } => {
                        QueryScope::Neighborhood { anchor, hops }
                    }
                },
                kind: request.kind,
                labels: request.labels,
                max_facts: request.max_facts,
                max_bytes: request.max_bytes,
                cursor: request.cursor.map(|cursor| QueryCursor {
                    snapshot_id: cursor.snapshot_id,
                    selection_id: cursor.selection_id,
                    offset: cursor.offset,
                }),
            },
        }
    }
}

impl From<QualifiedDiscovery> for QualifiedRequest<DiscoveryRequest> {
    fn from(value: QualifiedDiscovery) -> Self {
        let query = match value.request.query {
            StrictQuery::Identifier { id } => DiscoveryQuery::Identifier { id },
            StrictQuery::Name { name } => DiscoveryQuery::Name { name },
            StrictQuery::Text { text } => DiscoveryQuery::Text { text },
        };
        Self {
            context_id: value.context_id,
            request: DiscoveryRequest {
                query,
                labels: value.request.labels,
                max_candidates: value.request.max_candidates,
                max_bytes: value.request.max_bytes,
                cursor: value.request.cursor.map(|cursor| DiscoveryCursor {
                    snapshot_id: cursor.snapshot_id,
                    policy: cursor.policy,
                    query_id: cursor.query_id,
                    offset: cursor.offset,
                }),
            },
        }
    }
}

pub(super) fn parse<T: serde::de::DeserializeOwned>(
    arguments: Map<String, Value>,
) -> Result<T, ReadError> {
    serde_json::from_value(Value::Object(arguments)).map_err(|_| ReadError::InvalidRequest)
}
pub(super) fn success(id: &Value, summary: &'static str, data: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":summary}],"structuredContent":data,"isError":false}})
}
pub(super) fn tool_error(id: &Value, error: ReadError) -> Value {
    let mut detail = json!({"error":error.to_string()});
    if let ReadError::EvidenceOmitted(kind) = error {
        detail["omission_kind"] = json!(kind);
    }
    json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":error.to_string()}],"structuredContent":detail,"isError":true}})
}
pub(super) fn error_response(id: Option<&Value>, error: ReadError) -> Value {
    let code = match error {
        ReadError::InvalidRequest => -32602,
        ReadError::StaleBasis => -32001,
        ReadError::Limit => -32000,
        _ => -32603,
    };
    let mut response = json!({"jsonrpc":"2.0","error":{"code":code,"message":error.to_string()}});
    if let Some(id) = id {
        response["id"] = id.clone();
    }
    response
}
pub(super) fn inner_cap(template: &Value, cap: usize) -> Result<usize, ReadError> {
    let size = measure(template, cap).ok_or(ReadError::Limit)?;
    cap.checked_sub(size - 4)
        .filter(|inner| *inner > 0)
        .ok_or(ReadError::Limit)
}
pub(super) fn fits(value: &impl Serialize, cap: usize) -> bool {
    measure(value, cap).is_some()
}
fn measure(value: &impl Serialize, cap: usize) -> Option<usize> {
    struct Counter {
        remaining: usize,
        total: usize,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.remaining = self
                .remaining
                .checked_sub(bytes.len())
                .ok_or_else(|| io::Error::other("response limit"))?;
            self.total += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter {
        remaining: cap,
        total: 0,
    };
    serde_json::to_writer(&mut counter, value).ok()?;
    Some(counter.total)
}

fn limit_value(limit: ArtifactLimit) -> Value {
    match limit {
        ArtifactLimit::Trace(limit) => {
            json!({"stage":"trace","resource":match limit.limit {flowtracer::TraceLimit::Flows=>"flows",flowtracer::TraceLimit::Hops=>"hops",flowtracer::TraceLimit::OutputBytes=>"output_bytes"},"maximum":limit.maximum})
        }
        ArtifactLimit::Compiler(spec::SelectedArtifactLimit::OutputBytes { remaining }) => {
            json!({"stage":"compile","resource":"output_bytes","remaining":remaining})
        }
        ArtifactLimit::Compiler(spec::SelectedArtifactLimit::AdrDerivation(limit)) => {
            json!({"stage":"adr_derivation","resource":match limit.limit {spec::AdrLimit::Rows=>"rows",spec::AdrLimit::OutputBytes=>"output_bytes"},"maximum":limit.maximum})
        }
        ArtifactLimit::Compiler(spec::SelectedArtifactLimit::SecurityDerivation(limit)) => {
            json!({"stage":"security_derivation","resource":match limit.limit {spec::SecurityLimit::Rows=>"rows",spec::SecurityLimit::OutputBytes=>"output_bytes"},"maximum":limit.maximum})
        }
        // Not a size limit: the export's citations do not resolve (ADR-0058).
        ArtifactLimit::Compiler(spec::SelectedArtifactLimit::FactIndex(failure)) => {
            json!({"stage":"fact_index","resource":match failure {spec::FactIndexFailure::Unresolved=>"unresolved_citation",spec::FactIndexFailure::Collision=>"citation_id_collision",spec::FactIndexFailure::Withheld=>"withheld_citation"}})
        }
    }
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn session_properties() -> Value {
    json!({"session_id":{"type":"string"},"basis_id":{"type":"string"}})
}
pub(super) fn tool_descriptor(tool: ReadTool) -> Value {
    let (description, input_schema) = match tool {
        ReadTool::ReadFact => (
            "Unavailable in complete-session mode.",
            object_schema(json!({}), &[]),
        ),
        ReadTool::Capabilities => (
            "Describe this frozen recovered-context session and implemented limits.",
            object_schema(json!({}), &[]),
        ),
        ReadTool::Artifacts => (
            "List official artifact metadata without constructing content.",
            object_schema(session_properties(), &["session_id", "basis_id"]),
        ),
        ReadTool::Discover => {
            let query = json!({"oneOf":[object_schema(json!({"by":{"const":"identifier"},"id":{"type":"string"}}),&["by","id"]),object_schema(json!({"by":{"const":"name"},"name":{"type":"string"}}),&["by","name"]),object_schema(json!({"by":{"const":"text"},"text":{"type":"string"}}),&["by","text"])]});
            let cursor = object_schema(
                json!({"snapshot_id":{"type":"string"},"policy":{"type":"string"},"query_id":{"type":"string"},"offset":{"type":"integer","minimum":0}}),
                &["snapshot_id", "policy", "query_id", "offset"],
            );
            let request = object_schema(
                json!({"query":query,"labels":{"type":"array","items":{"type":"string"}},"max_candidates":{"type":"integer","minimum":1,"maximum":context_hub::discovery::MAX_CANDIDATES},"max_bytes":{"type":"integer","minimum":1,"maximum":context_hub::MAX_RESPONSE_BYTES,"description":"Maximum complete MCP response bytes, including qualification and protocol envelopes."},"cursor":{"anyOf":[cursor,{"type":"null"}]}}),
                &["query", "max_candidates", "max_bytes"],
            );
            let qualified = object_schema(
                json!({"request":request,"context_id":{"type":["string","null"]}}),
                &["request"],
            );
            let mut properties = session_properties();
            properties["request"] = qualified;
            (
                "Discover bounded recovered candidates under actual producer coverage.",
                object_schema(properties, &["session_id", "basis_id", "request"]),
            )
        }
        ReadTool::ReadArtifact => {
            let mut properties = session_properties();
            properties["artifact_id"] = json!({"type":"string","enum":super::artifacts::readable_catalog().into_iter().map(|entry|entry.id).collect::<Vec<_>>()});
            properties["expected_content_id"] = json!({"type":["string","null"],"description":"Required on nonzero offsets; binds exact complete artifact content."});
            properties["offset"] = json!({"type":"integer","minimum":0});
            properties["max_content_bytes"] = json!({"type":"integer","minimum":1});
            properties["max_bytes"] = json!({"type":"integer","minimum":1,"description":"Maximum complete MCP response bytes."});
            let mut schema = object_schema(
                properties,
                &[
                    "session_id",
                    "basis_id",
                    "artifact_id",
                    "offset",
                    "max_content_bytes",
                    "max_bytes",
                ],
            );
            schema["allOf"] = json!([{"if":{"properties":{"offset":{"minimum":1}}},"then":{"required":["expected_content_id"],"properties":{"expected_content_id":{"type":"string"}}}}]);
            (
                "Compile one official artifact and read identity-bound UTF-8 content pages.",
                schema,
            )
        }
        ReadTool::Evidence => evidence::descriptor(),
        ReadTool::Query => {
            let scope = json!({"oneOf":[
                object_schema(json!({"type":{"const":"all"}}),&["type"]),
                object_schema(json!({"type":{"const":"node"},"id":{"type":"string"}}),&["type","id"]),
                object_schema(json!({"type":{"const":"neighborhood"},"anchor":{"type":"string","description":"Exact node identity; its undirected neighborhood and the edges among those nodes are selected."},"hops":{"type":"integer","minimum":1,"maximum":context_hub::MAX_NEIGHBORHOOD_HOPS}}),&["type","anchor","hops"]),
            ]});
            let cursor = object_schema(
                json!({"snapshot_id":{"type":"string"},"selection_id":{"type":"string"},"offset":{"type":"integer","minimum":0}}),
                &["snapshot_id", "selection_id", "offset"],
            );
            let request = object_schema(
                json!({"scope":scope,"kind":{"enum":["node","edge",null]},"labels":{"type":"array","items":{"type":"string"},"description":"Exact labels; empty selects every label."},"max_facts":{"type":"integer","minimum":1,"maximum":context_hub::MAX_FACTS},"max_bytes":{"type":"integer","minimum":1,"maximum":context_hub::MAX_RESPONSE_BYTES,"description":"Maximum complete MCP response bytes, including qualification and protocol envelopes."},"cursor":{"anyOf":[cursor,{"type":"null"}]}}),
                &["scope", "max_facts", "max_bytes"],
            );
            let qualified = object_schema(
                json!({"request":request,"context_id":{"type":["string","null"],"description":"Required with a cursor: the context that issued it."}}),
                &["request"],
            );
            let mut properties = session_properties();
            properties["request"] = qualified;
            (
                "Read one bounded page of recovered facts: all facts, one exact node, or a node's induced neighborhood, with their relations.",
                object_schema(properties, &["session_id", "basis_id", "request"]),
            )
        }
    };
    json!({"name":tool.name(),"description":description,"inputSchema":input_schema,"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}})
}

#[cfg(all(test, not(app_test_group = "acceptance")))]
mod tests;
