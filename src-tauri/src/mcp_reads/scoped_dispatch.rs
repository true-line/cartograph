//! Explicit scoped MCP lifecycle; partial reads never become complete artifacts.
use super::{
    ReadError,
    dispatch::{
        DispatchAction, DispatchLimits, EvidenceWorkLimits, error_response, fits, inner_cap, parse,
        success, tool_error,
    },
    scoped_session::{
        ScopedDiscoveryRequest, ScopedReadContext, ScopedReadSession, ScopedRelationsRequest,
    },
};
use crate::AppState;
use core_graph::source::FactKey;
use mcp_wire::admission::{Action, Admission, ReadTool};
use serde::Deserialize;
use serde_json::{Map, Value, json};

pub(super) const TOOLS: [ReadTool; 7] = [
    ReadTool::Capabilities,
    ReadTool::Discover,
    ReadTool::ReadFact,
    ReadTool::Query,
    ReadTool::Evidence,
    ReadTool::Artifacts,
    ReadTool::ReadArtifact,
];
pub(crate) struct ScopedDispatcher<'a> {
    #[cfg(test)]
    admission: Admission,
    session: ScopedReadSession,
    session_id: String,
    limits: DispatchLimits,
    state: &'a AppState,
    evidence_limits: EvidenceWorkLimits,
    #[cfg(test)]
    closed: bool,
}
impl<'a> ScopedDispatcher<'a> {
    pub(crate) fn new(
        session: ScopedReadSession,
        session_id: String,
        limits: DispatchLimits,
        state: &'a AppState,
        evidence_limits: EvidenceWorkLimits,
    ) -> Result<Self, ReadError> {
        // Production session IDs are generated32-byte capabilities encoded as
        //64hex bytes. Keep direct callers within that same metadata envelope.
        if session_id.is_empty()
            || session_id.len() > 64
            || session_id.chars().any(char::is_control)
        {
            return Err(ReadError::InvalidRequest);
        }
        Ok(Self {
            #[cfg(test)]
            admission: Admission::new(TOOLS, limits.max_request_ids),
            session,
            session_id,
            limits,
            state,
            evidence_limits,
            #[cfg(test)]
            closed: false,
        })
    }
    #[cfg(test)]
    pub(crate) fn receive(
        &mut self,
        message: Value,
        ctx: &mut ScopedReadContext<'_>,
    ) -> DispatchAction {
        if self.closed {
            return DispatchAction::Close;
        }
        let action=match self.admission.receive(message) {
            Action::Ignore=>DispatchAction::Ignore,Action::Close(_)=>DispatchAction::Close,
            Action::Respond(value)=>self.bounded(value,self.limits.max_response_bytes.get()),
            Action::ListTools{id,tools}=>self.bounded(json!({"jsonrpc":"2.0","id":id,"result":{"tools":tools.into_iter().map(|t|schemas::descriptor(t,self.session.graph_limits())).collect::<Vec<_>>()}}),self.limits.max_response_bytes.get()),
            Action::Call {id,tool,arguments}=>self.call_admitted(&id,tool,arguments,ctx),
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
        ctx: &mut ScopedReadContext<'_>,
    ) -> DispatchAction {
        let requested = if tool == ReadTool::Discover {
            arguments.get("request").and_then(|r| r.get("max_bytes"))
        } else {
            arguments.get("max_bytes")
        };
        let cap = requested
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .filter(|v| *v > 0)
            .unwrap_or(self.limits.max_response_bytes.get())
            .min(self.limits.max_response_bytes.get());
        let result = self
            .call(id, tool, arguments, ctx, cap)
            .unwrap_or_else(|e| {
                if e == ReadError::InvalidRequest {
                    error_response(Some(id), e)
                } else {
                    tool_error(id, e)
                }
            });
        self.bounded(result, cap)
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
    fn check(&self, session: &str, basis: &str) -> Result<(), ReadError> {
        if session != self.session_id || basis != self.session.basis_id() {
            return Err(ReadError::StaleBasis);
        }
        Ok(())
    }
    fn envelope_cap(
        &self,
        id: &Value,
        summary: &'static str,
        field: &str,
        cap: usize,
    ) -> Result<usize, ReadError> {
        let mut data = Map::new();
        data.insert("session_id".into(), json!(self.session_id));
        data.insert(field.into(), Value::Null);
        inner_cap(&success(id, summary, Value::Object(data)), cap)
            .map(|n| n.min(context_hub::MAX_RESPONSE_BYTES))
    }
    fn wrap(
        &self,
        id: &Value,
        summary: &'static str,
        field: &str,
        body: impl serde::Serialize,
    ) -> Result<Value, ReadError> {
        let mut data = Map::new();
        data.insert("session_id".into(), json!(self.session_id));
        data.insert(
            field.into(),
            serde_json::to_value(body).map_err(|_| ReadError::Storage)?,
        );
        Ok(success(id, summary, Value::Object(data)))
    }
    fn call(
        &self,
        id: &Value,
        tool: ReadTool,
        args: Map<String, Value>,
        ctx: &mut ScopedReadContext<'_>,
        cap: usize,
    ) -> Result<Value, ReadError> {
        match tool {
            ReadTool::Query => {
                let args: Relations = parse(args)?;
                self.check(&args.session_id, &args.session_basis_id)?;
                let inner =
                    self.envelope_cap(id, "Scoped neighborhood relations.", "relations", cap)?;
                let request = ScopedRelationsRequest {
                    anchor: args.anchor,
                    hops: args.hops,
                    kind: args.kind,
                    labels: args.labels,
                    max_facts: args.max_facts,
                    max_bytes: inner,
                    cursor: args.cursor.map(|c| context_hub::QueryCursor {
                        snapshot_id: c.snapshot_id,
                        selection_id: c.selection_id,
                        offset: c.offset,
                    }),
                };
                let response = self
                    .session
                    .relations(ctx, &args.session_basis_id, request)?;
                self.wrap(id, "Scoped neighborhood relations.", "relations", response)
            }
            ReadTool::Capabilities => {
                let _: Empty = parse(args)?;
                self.session.check_current(ctx)?;
                Ok(success(
                    id,
                    "Scoped context capabilities.",
                    self.capabilities(),
                ))
            }
            ReadTool::Artifacts => {
                let args: Session = parse(args)?;
                self.check(&args.session_id, &args.session_basis_id)?;
                self.envelope_cap(id, "Scoped artifact availability.", "catalog", cap)?;
                self.session.check_current(ctx)?;
                self.wrap(
                    id,
                    "Scoped artifact availability.",
                    "catalog",
                    schemas::catalog(),
                )
            }
            ReadTool::Discover => {
                let args: Discover = parse(args)?;
                self.check(&args.session_id, &args.session_basis_id)?;
                let inner =
                    self.envelope_cap(id, "Scoped discovery candidates.", "discovery", cap)?;
                let request = ScopedDiscoveryRequest {
                    query: args.request.query.into(),
                    labels: args.request.labels,
                    max_scan_rows: args.request.max_scan_rows,
                    max_candidates: args.request.max_candidates,
                    max_bytes: inner,
                    cursor: args.request.cursor,
                };
                let response = self.session.discover(ctx, request)?;
                self.wrap(id, "Scoped discovery candidates.", "discovery", response)
            }
            ReadTool::ReadFact => {
                let args: Detail = parse(args)?;
                self.check(&args.session_id, &args.session_basis_id)?;
                let inner = self.envelope_cap(id, "Selected scoped fact.", "detail", cap)?;
                let response =
                    self.session
                        .detail(ctx, &args.session_basis_id, &args.fact.into(), inner)?;
                self.wrap(id, "Selected scoped fact.", "detail", response)
            }
            ReadTool::Evidence => {
                let args: Evidence = parse(args)?;
                match args {
                    Evidence::Describe {
                        session_id,
                        session_basis_id,
                        kind,
                        fact,
                        ordinal,
                        ..
                    } => {
                        self.check(&session_id, &session_basis_id)?;
                        if ordinal >= adapters_lang_ts::captured::MAX_RECEIPT_RANGES {
                            return Err(ReadError::InvalidRequest);
                        }
                        let inner = self.envelope_cap(
                            id,
                            "Scoped retained evidence descriptor.",
                            "evidence",
                            cap,
                        )?;
                        let proof = self.session.authorize_evidence(
                            ctx,
                            &session_basis_id,
                            &fact.into(),
                        )?;
                        let response = super::scoped_evidence::describe(
                            self.state,
                            &proof,
                            kind.role(),
                            ordinal,
                            inner,
                            || self.session.check_current(ctx),
                        )?;
                        self.wrap(
                            id,
                            "Scoped retained evidence descriptor.",
                            "evidence",
                            response,
                        )
                    }
                    Evidence::Read {
                        session_id,
                        session_basis_id,
                        kind,
                        target,
                        ..
                    } => {
                        self.check(&session_id, &session_basis_id)?;
                        let inner = self.envelope_cap(
                            id,
                            "Scoped retained primary source.",
                            "evidence",
                            cap,
                        )?;
                        let target = super::scoped_evidence::ScopedEvidenceTarget::decode(&target)?;
                        // The kind names the role the target cites.
                        if target.binding_role() != kind.role() {
                            return Err(ReadError::InvalidRequest);
                        }
                        let key = crate::task_evidence::graph_fact(&target.fact);
                        let proof =
                            self.session
                                .authorize_evidence(ctx, &session_basis_id, &key)?;
                        let response = super::scoped_evidence::read(
                            self.state,
                            &proof,
                            &target,
                            super::evidence::EvidenceLimits {
                                max_file_validation_bytes: self
                                    .evidence_limits
                                    .max_file_validation_bytes
                                    .get(),
                                max_span_bytes: self.evidence_limits.max_span_bytes.get(),
                                max_response_bytes: inner,
                            },
                            || self.session.check_current(ctx),
                        )?;
                        self.wrap(id, "Scoped retained primary source.", "evidence", response)
                    }
                }
            }
            ReadTool::ReadArtifact => {
                let args: Artifact = parse(args)?;
                self.check(&args.session_id, &args.session_basis_id)?;
                let inner = self
                    .envelope_cap(id, "Closed scoped artifact.", "partition", cap)?
                    .min(self.limits.compile.max_bytes);
                let response = self.session.partition(
                    ctx,
                    &args.session_basis_id,
                    args.kind.into(),
                    &args.root,
                    inner,
                )?;
                self.wrap(id, "Closed scoped artifact.", "partition", response)
            }
        }
    }
    fn capabilities(&self) -> Value {
        schemas::capabilities(
            &self.session_id,
            self.session.basis_id(),
            self.session.revision(),
            &self.session.coverage().coverage_id,
            self.session.graph_limits(),
            self.limits,
            self.evidence_limits,
        )
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Session {
    session_id: String,
    session_basis_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Discover {
    session_id: String,
    session_basis_id: String,
    request: Discovery,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Discovery {
    query: Query,
    labels: Vec<String>,
    max_scan_rows: usize,
    max_candidates: usize,
    #[serde(rename = "max_bytes")]
    _max_bytes: std::num::NonZeroUsize,
    cursor: Option<super::scoped_session::ScopedCursor>,
}
#[derive(Deserialize)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
enum Query {
    Identifier { id: String },
    Name { name: String },
    Text { text: String },
}
impl From<Query> for context_hub::DiscoveryQuery {
    fn from(q: Query) -> Self {
        match q {
            Query::Identifier { id } => Self::Identifier { id },
            Query::Name { name } => Self::Name { name },
            Query::Text { text } => Self::Text { text },
        }
    }
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Fact {
    Node {
        id: String,
    },
    Edge {
        source: String,
        label: String,
        destination: String,
    },
}
impl From<Fact> for FactKey {
    fn from(f: Fact) -> Self {
        match f {
            Fact::Node { id } => Self::Node { id },
            Fact::Edge {
                source,
                label,
                destination,
            } => Self::Edge {
                source,
                label,
                destination,
            },
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Detail {
    session_id: String,
    session_basis_id: String,
    fact: Fact,
    #[serde(rename = "max_bytes")]
    _max_bytes: std::num::NonZeroUsize,
}
/// A neighborhood of one exact node (AC-1810). Every budget is required and
/// no unknown field is accepted; one node is `cartograph_read_fact`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Relations {
    session_id: String,
    session_basis_id: String,
    anchor: String,
    hops: usize,
    #[serde(default)]
    kind: Option<context_hub::FactKind>,
    #[serde(default)]
    labels: Vec<String>,
    max_facts: usize,
    #[serde(rename = "max_bytes")]
    _max_bytes: std::num::NonZeroUsize,
    #[serde(default)]
    cursor: Option<RelationsCursor>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RelationsCursor {
    snapshot_id: String,
    selection_id: String,
    offset: usize,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum EvidenceKind {
    RetainedPrimarySource,
    /// A fact's observed binding: a captured observation input (ADR-0079).
    RetainedObservedInput,
}
impl EvidenceKind {
    fn role(&self) -> core_graph::BindingRole {
        match self {
            Self::RetainedPrimarySource => core_graph::BindingRole::Primary,
            Self::RetainedObservedInput => core_graph::BindingRole::Observed,
        }
    }
}
#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum Evidence {
    Describe {
        session_id: String,
        session_basis_id: String,
        kind: EvidenceKind,
        fact: Fact,
        ordinal: usize,
        #[serde(rename = "max_bytes")]
        _max_bytes: std::num::NonZeroUsize,
    },
    Read {
        session_id: String,
        session_basis_id: String,
        kind: EvidenceKind,
        target: Value,
        #[serde(rename = "max_bytes")]
        _max_bytes: std::num::NonZeroUsize,
    },
}
mod logical;
pub(super) mod schemas;
pub(crate) use logical::logical_protocol_workspace_bytes;
#[cfg(all(test, not(app_test_group = "acceptance")))]
mod tests;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ArtifactKind {
    DataModel,
    RuleEvidence,
    RuleEvidenceIndex,
}
impl From<ArtifactKind> for spec::PartitionArtifactKind {
    fn from(kind: ArtifactKind) -> Self {
        match kind {
            ArtifactKind::DataModel => Self::DataModel,
            ArtifactKind::RuleEvidence => Self::RuleEvidence,
            ArtifactKind::RuleEvidenceIndex => Self::RuleEvidenceIndex,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    session_id: String,
    session_basis_id: String,
    kind: ArtifactKind,
    root: String,
    #[serde(rename = "max_bytes")]
    _max_bytes: std::num::NonZeroUsize,
}
