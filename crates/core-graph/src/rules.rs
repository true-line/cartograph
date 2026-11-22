//! Shared source-rule evidence data contract (SPEC-02, ADR-0020).
//!
//! Store [`GuardedExitEvidence`] under `BusinessRule.props.rule`; the node's
//! ordinary provenance remains separate. These types describe local source
//! observations, not established business validations or execution predicates.
//! They perform no source reads, evaluation, redaction, or graph lookup.
//!
//! Producers must sanitize every stored display and literal value before
//! constructing a fact. [`GuardedExitEvidence::from_value`] checks wire shape,
//! version, and source spans, but cannot prove sanitization, callable ownership,
//! or the existence of cited code. Structs alone do not implement AC-0122–0125.

use core_prov::EvidenceRef;
use serde::{Deserialize, Serialize};

/// Supported version of the source-rule payload, independent of graph storage.
pub const RULE_EVIDENCE_SCHEMA_VERSION: u32 = 2;

mod definitions;
pub use definitions::{
    BinaryOperator, DefinitionDependency, DefinitionExpression, DefinitionExpressionKind,
    DefinitionExpressionNode, LocalDefinition, LogicalOperator, MAX_DEFINITION_CHAIN_DEPTH,
    MAX_DEFINITION_EXPRESSION_DEPTH, MAX_DEFINITION_NODES, MAX_INITIALIZER_DEPENDENCIES,
    MAX_LOCAL_DEFINITIONS, MAX_RULE_PAYLOAD_BYTES, UnaryOperator,
};
#[cfg(test)]
mod definition_tests;
mod workspace;
pub use workspace::{decoded_heap_bytes, validation_workspace, validation_workspace_bytes};

/// An observed guarded local exit, with explicitly incomplete interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardedExitEvidence {
    /// Payload contract version; currently [`RULE_EVIDENCE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The source observation represented by this payload.
    pub kind: RuleKind,
    /// Identity of the actual emitted callable that owns the exit.
    pub owner_id: String,
    /// Original nonempty span of the return or throw statement.
    pub exit_source: EvidenceRef,
    /// Zero-based lexical exit order within this callable, not runtime order.
    pub source_order: u32,
    /// Same-callable branch ancestors, outermost first; not a path predicate.
    pub conditions: Vec<BranchCondition>,
    /// Observed local return or throw, without assigning consumer meaning.
    pub effect: LocalExit,
    /// Cited prerequisites and explicit unresolved dependency references.
    pub dependencies: Vec<RuleDependency>,
    /// Behavioral interpretation remains unestablished in this schema version.
    pub interpretation: Interpretation,
    /// Withheld source locations and fixed reasons, never original secret text.
    pub redactions: Vec<SourceRedaction>,
    /// Cited initializer evidence; absent for v1, required (possibly empty) for v2.
    /// A present null is rejected rather than normalized to absent history.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "definitions::present_definitions"
    )]
    pub local_definitions: Option<Vec<LocalDefinition>>,
}

/// Supported source observation kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    /// A return or throw observed within source branch conditions.
    GuardedExit,
    /// A state change or call statement, with the source branch conditions
    /// around it, possibly none (#1998, ADR-0020 amended): an assignment
    /// through an attribute or subscript, or a call whose result is unused.
    GuardedEffect,
}

/// One lexical branch condition governing the source location of an exit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchCondition {
    /// Original span of the enclosing branch statement.
    pub branch_source: EvidenceRef,
    /// Source expression, preserving operators and short-circuit structure.
    pub expression: SourceExpression,
    /// Whether this source location is in the truthy or falsy branch.
    pub polarity: BranchPolarity,
}

/// JavaScript branch polarity without converting truthiness to equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchPolarity {
    /// The branch selected when its expression is truthy.
    TruthyBranch,
    /// The branch selected when its expression is falsy.
    FalsyBranch,
    /// A handler selected when the guarded body raises one of the error
    /// types its expression names (Python `except T:`, AC-1036).
    RaisedBranch,
    /// The guarded body itself, reached only while none of the error types
    /// its handler's expression names is raised (AC-1036).
    UnraisedBranch,
}

/// Sanitized display text supplied by a source-aware producer.
///
/// This wrapper marks the producer obligation; it does not verify redaction.
/// Deserialization accepts a string and likewise does not sanitize its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SanitizedDisplay(String);

impl SanitizedDisplay {
    /// Wrap text after the producer has applied the source-redaction policy.
    pub fn from_sanitized(text: String) -> Self {
        Self(text)
    }

    /// Borrow the already-sanitized display without reading source evidence.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An expression observed in source, with capture limits separate from meaning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceExpression {
    /// Original nonempty source span; display substitutions never shift it.
    pub source: EvidenceRef,
    /// Parser syntax kind, such as `binary_expression` or `false`.
    pub syntax_kind: String,
    /// Safe display, never an automatic raw evidence dereference.
    pub display: SanitizedDisplay,
    /// Syntactic capture status, not behavioral confidence or completeness.
    pub capture: ExpressionCapture,
    /// Typed value information when the whole expression is a literal.
    pub literal: Option<LiteralEvidence>,
}

/// How much source syntax survived capture into the stored expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpressionCapture {
    /// Complete source syntax, without claiming a complete execution predicate.
    CompleteSyntax,
    /// Some source content was deliberately withheld.
    Redacted,
    /// The source representation could not be captured safely or completely.
    Unsupported,
}

/// Literal value knowledge, retaining a withheld value's syntactic type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum LiteralEvidence {
    /// A typed value whose storage the source-aware policy permits.
    Known {
        /// Safe scalar value; a boolean false is never the string `"false"`.
        value: KnownLiteral,
    },
    /// A typed literal with no stored original value or secret-only digest.
    Withheld {
        /// Literal type still established from syntax.
        kind: LiteralKind,
        /// Nonempty fixed reasons explaining the lost value.
        reasons: Vec<RedactionReason>,
    },
}

/// Supported known scalar literals. Number values are finite JSON numbers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum KnownLiteral {
    /// Boolean literal, preserving false independently of text redaction.
    Boolean(bool),
    /// String whose value has passed the producer's storage policy.
    String(String),
    /// Representable finite numeric literal; unsupported forms stay uncaptured.
    Number(serde_json::Number),
    /// The literal null, without treating an absent return argument as null.
    Null,
}

/// Syntactic scalar type retained when its value is withheld.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiteralKind {
    /// Boolean literal.
    Boolean,
    /// String literal.
    String,
    /// Numeric literal.
    Number,
    /// Null literal.
    Null,
}

/// A source-observed local effect, without interpreting the caller's behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LocalExit {
    /// Local return; no argument is distinct from returning a literal null.
    Return {
        /// Returned source expression, or none for a bare return statement.
        value: Option<SourceExpression>,
    },
    /// Local throw; exception handling or finally may alter its overall effect.
    Throw {
        /// Thrown source expression.
        value: SourceExpression,
    },
    /// Assignment through an attribute or subscript (`alias.enabled = False`,
    /// `g.user = api_key.user`); the callable continues (#1998).
    Assign {
        /// The assigned place.
        target: SourceExpression,
        /// The assigned source expression.
        value: SourceExpression,
    },
    /// A call statement whose result is unused; the callable continues (#1998).
    Call {
        /// The call expression.
        value: SourceExpression,
    },
}

impl LocalExit {
    /// The effect's source expressions in source order.
    pub fn expressions(&self) -> Vec<&SourceExpression> {
        match self {
            LocalExit::Return { value } => value.iter().collect(),
            LocalExit::Throw { value } | LocalExit::Call { value } => vec![value],
            LocalExit::Assign { target, value } => vec![target, value],
        }
    }

    /// The effect's source expressions in source order, mutably.
    pub fn expressions_mut(&mut self) -> Vec<&mut SourceExpression> {
        match self {
            LocalExit::Return { value } => value.iter_mut().collect(),
            LocalExit::Throw { value } | LocalExit::Call { value } => vec![value],
            LocalExit::Assign { target, value } => vec![target, value],
        }
    }

    /// Whether the effect leaves the callable (a return or throw), as
    /// [`RuleKind::GuardedExit`] requires; an assignment or call does not and
    /// is a [`RuleKind::GuardedEffect`].
    pub fn exits(&self) -> bool {
        matches!(self, LocalExit::Return { .. } | LocalExit::Throw { .. })
    }
}

/// A prerequisite relevant to a source rule's conditions or local effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDependency {
    /// Why the prerequisite matters to this source observation.
    pub role: DependencyRole,
    /// Original nonempty span establishing the dependency observation.
    pub source: EvidenceRef,
    /// Scope-proven binding/target or the identity of an explicit graph Gap.
    pub resolution: DependencyResolution,
}

/// Position of a prerequisite within the source-rule observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyRole {
    /// Input to an enclosing branch expression.
    Condition,
    /// Input to the returned or thrown expression.
    ExitValue,
    /// Control flow affecting reachability, ordering, or consumer behavior.
    ControlFlow,
}

/// What source analysis established about one dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DependencyResolution {
    /// A lexical declaration whose binding was proven within scope.
    Binding {
        /// Stable scope/declaration identity; not necessarily a graph node.
        binding_id: String,
        /// Original nonempty declaration span.
        declaration: EvidenceRef,
    },
    /// A proven, actually emitted graph target.
    Target {
        /// Graph node identity; existence must be checked by the producer.
        node_id: String,
    },
    /// An explicitly unresolved dependency, represented by an actual Gap node.
    Unresolved {
        /// Graph Gap identity; the producer also emits its DEPENDS_ON edge.
        gap_id: String,
    },
}

/// Explicit limits of source evidence; this schema cannot assert business truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interpretation {
    /// A lexical ancestor list is not a complete execution predicate.
    pub execution_predicate: InterpretationStatus,
    /// A returned string does not establish rejection or validation semantics.
    pub consumer_effect: InterpretationStatus,
    /// Explicit graph gaps affecting interpretation, in deterministic order.
    pub gap_ids: Vec<String>,
}

/// Interpretation supported by this source-observation schema version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterpretationStatus {
    /// No complete behavioral claim has been established.
    NotEstablished,
}

/// A withheld original source location and a fixed explanation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRedaction {
    /// Original nonempty source span; no secret value is stored here.
    pub source: EvidenceRef,
    /// Why its text or decoded value was withheld.
    pub reason: RedactionReason,
}

/// Fixed redaction reasons; diagnostics must not contain raw source fragments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionReason {
    /// Private-key block recognized in a decoded value.
    PrivateKey,
    /// Provider-specific credential token.
    ProviderToken,
    /// Bearer authorization token.
    BearerToken,
    /// AWS access-key identifier.
    AwsAccessKey,
    /// Credential-assignment text or an AST-proven sensitive binding/property value.
    CredentialValue,
    /// Value matched the conservative compact-token shape policy.
    TokenShaped,
    /// Literal decoding could not safely preserve a source value.
    UnsupportedDecoding,
    /// The expression contains syntax outside the safe capture policy.
    UnsupportedSyntax,
    /// Comment content was deliberately omitted from stored expressions.
    RemovedComment,
}

/// Fixed reasons for source-rule interpretation/dependency Gap nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleGapReason {
    /// Initializer syntax has no supported structural representation.
    LocalDefinitionUnsupported,
    /// A local definition or traversal budget was exhausted.
    DefinitionLimit,
    /// A read's runtime value has not been established.
    RuntimeValueUnknown,
    /// Structured declaration-before-use proof was unavailable.
    DefinitionOrderUnproven,
    /// Definition and use cross an unsupported execution/control scope.
    DefinitionScopeUnsupported,
    /// The binding is outside the admitted directly initialized const subset.
    DefinitionBindingUnsupported,
    /// Parser recovery could not produce a valid cited source observation.
    UnsupportedRuleSyntax,
    /// A condition, expression, rule or per-file analysis bound was reached.
    AnalysisLimit,
    /// Full reachability and execution predicates have not been established.
    ExecutionPredicateUnknown,
    /// A referenced value has no scope-proven stable declaration.
    UnresolvedBinding,
    /// The bounded dependency observation budget was exhausted.
    DependencyLimit,
    /// Decoded eval offsets cannot yet be cited as original expression spans.
    EvalSourceMappingUnknown,
    /// A dependency call target or its effect is unresolved.
    UnresolvedCall,
    /// An earlier exit can affect reachability.
    PrecedingExit,
    /// Mutation can affect the values entering this source observation.
    Mutation,
    /// Loop behavior or an accumulator relationship is not established.
    LoopDependency,
    /// Switch control flow has not been interpreted.
    SwitchControl,
    /// Exception handling or finally behavior has not been interpreted.
    ExceptionControl,
    /// Withheld expression content limits interpretation.
    RedactedExpression,
    /// The caller's interpretation of this local effect is unknown.
    ConsumerSemanticsUnknown,
}

impl RuleGapReason {
    /// The language-neutral cause a gap register prints for this reason. An
    /// adapter may word a reason in its own language first; every other
    /// reason falls back here, so no reason an adapter emits reads as the
    /// generic "remains incomplete" line (#1774).
    pub fn description(self) -> &'static str {
        match self {
            Self::LocalDefinitionUnsupported => {
                "The initializer contains source forms outside supported structural capture."
            }
            Self::DefinitionLimit => {
                "A local-definition capture limit was reached; initializer evidence remains incomplete."
            }
            Self::RuntimeValueUnknown => {
                "The value of this runtime input or member read has not been established."
            }
            Self::DefinitionOrderUnproven => {
                "Declaration initialization before this original read was not established."
            }
            Self::DefinitionScopeUnsupported => {
                "Initializer recovery crosses an unsupported callable or control-flow boundary."
            }
            Self::DefinitionBindingUnsupported => {
                "This binding is not a stable, directly initialized local."
            }
            Self::UnsupportedRuleSyntax => {
                "Incomplete source syntax prevented supported rule analysis; remaining file analysis was omitted."
            }
            Self::AnalysisLimit => {
                "A source-rule capture limit was reached; additional analysis remains incomplete."
            }
            Self::ExecutionPredicateUnknown => {
                "The complete execution predicate has not been established."
            }
            Self::UnresolvedBinding => {
                "A value prerequisite lacks a stable scope-proven declaration."
            }
            Self::DependencyLimit => {
                "The dependency observation limit was reached; additional prerequisites remain unanalyzed."
            }
            Self::EvalSourceMappingUnknown => {
                "Decoded eval offsets cannot yet be cited as original expression spans."
            }
            Self::UnresolvedCall => {
                "A call prerequisite lacks a proven target or behavioral interpretation."
            }
            Self::PrecedingExit => {
                "An earlier exit is present; its effect on reachability has not been analyzed."
            }
            Self::Mutation => {
                "Mutation is present in the callable; value changes have not been interpreted."
            }
            Self::LoopDependency => {
                "Loop or accumulator behavior in the callable has not been interpreted."
            }
            Self::SwitchControl => {
                "Branching control flow in the callable has not been interpreted."
            }
            Self::ExceptionControl => {
                "Exception or error handling in the callable has not been interpreted."
            }
            Self::RedactedExpression => "An expression was withheld or incompletely captured.",
            Self::ConsumerSemanticsUnknown => {
                "The caller's interpretation of this local exit is unknown."
            }
        }
    }

    /// [`Self::description`] for a stored reason key; an unknown key keeps
    /// the generic line.
    pub fn describe_key(key: &str) -> &'static str {
        serde_json::from_value::<Self>(serde_json::Value::String(key.into()))
            .map(Self::description)
            .unwrap_or("Source-rule interpretation remains incomplete.")
    }
}

/// Shape-validation failure without incorporating arbitrary source text.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RuleValidationError {
    /// JSON does not match the known typed contract.
    #[error("malformed source-rule payload")]
    MalformedPayload,
    /// The payload uses an unsupported schema version.
    #[error("unsupported source-rule schema version: {0}")]
    UnsupportedVersion(u32),
    /// A cited source span is empty or reversed.
    #[error("invalid source-rule span in {0}")]
    InvalidSourceSpan(&'static str),
    /// A reference has an empty identity.
    #[error("empty source-rule identity in {0}")]
    EmptyIdentity(&'static str),
    /// A withheld value has no fixed explanation.
    #[error("withheld source-rule literal has no reason")]
    MissingWithholdingReason,
    /// A withheld literal cannot claim to have retained complete syntax.
    #[error("withheld source-rule literal claims complete syntax")]
    InconsistentCapture,
    /// A bounded v2 collection, text, depth or serialized-size limit was exceeded.
    #[error("source-rule limit exceeded: {0}")]
    Limit(&'static str),
    /// A v2 arena, declaration or reference is internally inconsistent.
    #[error("invalid source-rule definition: {0}")]
    InvalidDefinition(&'static str),
}

impl GuardedExitEvidence {
    /// Visit every original source reference in deterministic structural order.
    ///
    /// Re-ingestion can retarget revisions through this one traversal without
    /// leaving nested expression, dependency, or redaction evidence stale.
    /// This changes neither fact identities nor source text and performs no
    /// reads; callers validate the result and recompute content hashes when
    /// their canonical payload includes the changed references.
    pub fn visit_sources_mut(&mut self, mut visitor: impl FnMut(&mut EvidenceRef)) {
        visitor(&mut self.exit_source);
        for condition in &mut self.conditions {
            visitor(&mut condition.branch_source);
            visitor(&mut condition.expression.source);
        }
        for value in self.effect.expressions_mut() {
            visitor(&mut value.source);
        }
        for dependency in &mut self.dependencies {
            visitor(&mut dependency.source);
            if let DependencyResolution::Binding { declaration, .. } = &mut dependency.resolution {
                visitor(declaration);
            }
        }
        if let Some(definitions) = &mut self.local_definitions {
            for definition in definitions {
                visitor(&mut definition.declaration);
                for usage in &mut definition.uses {
                    visitor(usage);
                }
                visitor(&mut definition.initializer.source);
                for node in &mut definition.expression.nodes {
                    visitor(&mut node.expression.source);
                }
                for dependency in &mut definition.dependencies {
                    visitor(&mut dependency.source);
                    if let DependencyResolution::Binding { declaration, .. } =
                        &mut dependency.resolution
                    {
                        visitor(declaration);
                    }
                }
            }
        }
        for redaction in &mut self.redactions {
            visitor(&mut redaction.source);
        }
    }

    /// Read and validate a known rule payload without reading its source.
    ///
    /// Malformed JSON diagnostics are deliberately fixed rather than echoing
    /// unexpected input values. Deserializing with serde directly is possible,
    /// but callers must then invoke [`Self::validate`] before using the fact.
    pub fn from_value(value: serde_json::Value) -> Result<Self, RuleValidationError> {
        let version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .ok_or(RuleValidationError::MalformedPayload)?;
        match version {
            1 => {
                if value.get("local_definitions").is_some() {
                    return Err(RuleValidationError::MalformedPayload);
                }
            }
            2 => definitions::preflight(&value)?,
            other => return Err(RuleValidationError::UnsupportedVersion(other)),
        }
        let rule: Self =
            serde_json::from_value(value).map_err(|_| RuleValidationError::MalformedPayload)?;
        rule.validate()?;
        Ok(rule)
    }

    /// Validate version, nested nonempty source spans, and basic shape coherence.
    ///
    /// This does not verify sanitization, cited source contents, graph target
    /// existence, scope membership, or behavioral correctness.
    pub fn validate(&self) -> Result<(), RuleValidationError> {
        match self.schema_version {
            1 if self.local_definitions.is_none() => {}
            1 => return Err(RuleValidationError::MalformedPayload),
            2 => definitions::collection_bounds(self)?,
            version => return Err(RuleValidationError::UnsupportedVersion(version)),
        }
        identity(&self.owner_id, "owner_id")?;
        span(&self.exit_source, "exit_source")?;
        for condition in &self.conditions {
            span(&condition.branch_source, "branch_source")?;
            expression(&condition.expression)?;
        }
        // An exit is a guarded exit; an assignment or call a guarded effect.
        if self.effect.exits() != (self.kind == RuleKind::GuardedExit) {
            return Err(RuleValidationError::MalformedPayload);
        }
        for value in self.effect.expressions() {
            expression(value)?;
        }
        for dependency in &self.dependencies {
            span(&dependency.source, "dependency.source")?;
            match &dependency.resolution {
                DependencyResolution::Binding {
                    binding_id,
                    declaration,
                } => {
                    identity(binding_id, "binding_id")?;
                    span(declaration, "binding.declaration")?;
                }
                DependencyResolution::Target { node_id } => identity(node_id, "target.node_id")?,
                DependencyResolution::Unresolved { gap_id } => {
                    identity(gap_id, "dependency.gap_id")?
                }
            }
        }
        for gap_id in &self.interpretation.gap_ids {
            identity(gap_id, "interpretation.gap_id")?;
        }
        for redaction in &self.redactions {
            span(&redaction.source, "redaction.source")?;
        }
        if self.schema_version == 2 {
            definitions::validate(self)?;
        }
        Ok(())
    }
}

fn identity(value: &str, field: &'static str) -> Result<(), RuleValidationError> {
    if value.is_empty() {
        return Err(RuleValidationError::EmptyIdentity(field));
    }
    Ok(())
}

fn span(source: &EvidenceRef, field: &'static str) -> Result<(), RuleValidationError> {
    if source.byte_start >= source.byte_end {
        return Err(RuleValidationError::InvalidSourceSpan(field));
    }
    Ok(())
}

fn expression(value: &SourceExpression) -> Result<(), RuleValidationError> {
    span(&value.source, "expression.source")?;
    if let Some(LiteralEvidence::Withheld { reasons, .. }) = &value.literal {
        if reasons.is_empty() {
            return Err(RuleValidationError::MissingWithholdingReason);
        }
        if value.capture == ExpressionCapture::CompleteSyntax {
            return Err(RuleValidationError::InconsistentCapture);
        }
    }
    Ok(())
}

/// The status code an `http::StatusCode` associated constant names, by its
/// name (`NOT_FOUND` is 404), every constant `http` 1.x defines; `None` for
/// any other name (#2018).
pub fn rust_http_status(name: &str) -> Option<u16> {
    const STATUSES: &[(&str, u16)] = &[
        ("CONTINUE", 100),
        ("SWITCHING_PROTOCOLS", 101),
        ("PROCESSING", 102),
        ("EARLY_HINTS", 103),
        ("OK", 200),
        ("CREATED", 201),
        ("ACCEPTED", 202),
        ("NON_AUTHORITATIVE_INFORMATION", 203),
        ("NO_CONTENT", 204),
        ("RESET_CONTENT", 205),
        ("PARTIAL_CONTENT", 206),
        ("MULTI_STATUS", 207),
        ("ALREADY_REPORTED", 208),
        ("IM_USED", 226),
        ("MULTIPLE_CHOICES", 300),
        ("MOVED_PERMANENTLY", 301),
        ("FOUND", 302),
        ("SEE_OTHER", 303),
        ("NOT_MODIFIED", 304),
        ("USE_PROXY", 305),
        ("TEMPORARY_REDIRECT", 307),
        ("PERMANENT_REDIRECT", 308),
        ("BAD_REQUEST", 400),
        ("UNAUTHORIZED", 401),
        ("PAYMENT_REQUIRED", 402),
        ("FORBIDDEN", 403),
        ("NOT_FOUND", 404),
        ("METHOD_NOT_ALLOWED", 405),
        ("NOT_ACCEPTABLE", 406),
        ("PROXY_AUTHENTICATION_REQUIRED", 407),
        ("REQUEST_TIMEOUT", 408),
        ("CONFLICT", 409),
        ("GONE", 410),
        ("LENGTH_REQUIRED", 411),
        ("PRECONDITION_FAILED", 412),
        ("PAYLOAD_TOO_LARGE", 413),
        ("URI_TOO_LONG", 414),
        ("UNSUPPORTED_MEDIA_TYPE", 415),
        ("RANGE_NOT_SATISFIABLE", 416),
        ("EXPECTATION_FAILED", 417),
        ("IM_A_TEAPOT", 418),
        ("MISDIRECTED_REQUEST", 421),
        ("UNPROCESSABLE_ENTITY", 422),
        ("LOCKED", 423),
        ("FAILED_DEPENDENCY", 424),
        ("TOO_EARLY", 425),
        ("UPGRADE_REQUIRED", 426),
        ("PRECONDITION_REQUIRED", 428),
        ("TOO_MANY_REQUESTS", 429),
        ("REQUEST_HEADER_FIELDS_TOO_LARGE", 431),
        ("UNAVAILABLE_FOR_LEGAL_REASONS", 451),
        ("INTERNAL_SERVER_ERROR", 500),
        ("NOT_IMPLEMENTED", 501),
        ("BAD_GATEWAY", 502),
        ("SERVICE_UNAVAILABLE", 503),
        ("GATEWAY_TIMEOUT", 504),
        ("HTTP_VERSION_NOT_SUPPORTED", 505),
        ("VARIANT_ALSO_NEGOTIATES", 506),
        ("INSUFFICIENT_STORAGE", 507),
        ("LOOP_DETECTED", 508),
        ("NOT_EXTENDED", 510),
        ("NETWORK_AUTHENTICATION_REQUIRED", 511),
    ];
    STATUSES
        .iter()
        .find(|(constant, _)| *constant == name)
        .map(|(_, code)| *code)
}

/// The status code a Go `net/http` status constant names, by its name
/// without the package (`StatusForbidden` is 403); `None` for any other
/// name (#2009).
pub fn go_http_status(name: &str) -> Option<u16> {
    Some(match name.strip_prefix("Status")? {
        "Continue" => 100,
        "SwitchingProtocols" => 101,
        "Processing" => 102,
        "EarlyHints" => 103,
        "OK" => 200,
        "Created" => 201,
        "Accepted" => 202,
        "NonAuthoritativeInfo" => 203,
        "NoContent" => 204,
        "ResetContent" => 205,
        "PartialContent" => 206,
        "MultiStatus" => 207,
        "AlreadyReported" => 208,
        "IMUsed" => 226,
        "MultipleChoices" => 300,
        "MovedPermanently" => 301,
        "Found" => 302,
        "SeeOther" => 303,
        "NotModified" => 304,
        "UseProxy" => 305,
        "TemporaryRedirect" => 307,
        "PermanentRedirect" => 308,
        "BadRequest" => 400,
        "Unauthorized" => 401,
        "PaymentRequired" => 402,
        "Forbidden" => 403,
        "NotFound" => 404,
        "MethodNotAllowed" => 405,
        "NotAcceptable" => 406,
        "ProxyAuthRequired" => 407,
        "RequestTimeout" => 408,
        "Conflict" => 409,
        "Gone" => 410,
        "LengthRequired" => 411,
        "PreconditionFailed" => 412,
        "RequestEntityTooLarge" => 413,
        "RequestURITooLong" => 414,
        "UnsupportedMediaType" => 415,
        "RequestedRangeNotSatisfiable" => 416,
        "ExpectationFailed" => 417,
        "Teapot" => 418,
        "MisdirectedRequest" => 421,
        "UnprocessableEntity" => 422,
        "Locked" => 423,
        "FailedDependency" => 424,
        "TooEarly" => 425,
        "UpgradeRequired" => 426,
        "PreconditionRequired" => 428,
        "TooManyRequests" => 429,
        "RequestHeaderFieldsTooLarge" => 431,
        "UnavailableForLegalReasons" => 451,
        "InternalServerError" => 500,
        "NotImplemented" => 501,
        "BadGateway" => 502,
        "ServiceUnavailable" => 503,
        "GatewayTimeout" => 504,
        "HTTPVersionNotSupported" => 505,
        "VariantAlsoNegotiates" => 506,
        "InsufficientStorage" => 507,
        "LoopDetected" => 508,
        "NotExtended" => 510,
        "NetworkAuthenticationRequired" => 511,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // #1774: every stored reason key has its own cause line; only an unknown
    // key prints the generic one.
    #[test]
    fn every_rule_gap_reason_key_has_its_own_description() {
        for key in [
            "local_definition_unsupported",
            "definition_limit",
            "runtime_value_unknown",
            "definition_order_unproven",
            "definition_scope_unsupported",
            "definition_binding_unsupported",
            "unsupported_rule_syntax",
            "analysis_limit",
            "execution_predicate_unknown",
            "unresolved_binding",
            "dependency_limit",
            "eval_source_mapping_unknown",
            "unresolved_call",
            "preceding_exit",
            "mutation",
            "loop_dependency",
            "switch_control",
            "exception_control",
            "redacted_expression",
            "consumer_semantics_unknown",
        ] {
            let reason: RuleGapReason =
                serde_json::from_value(serde_json::Value::String(key.into())).unwrap();
            assert_eq!(
                RuleGapReason::describe_key(key),
                reason.description(),
                "{key}"
            );
            assert_ne!(
                reason.description(),
                "Source-rule interpretation remains incomplete.",
                "{key}"
            );
        }
        assert_eq!(
            RuleGapReason::describe_key("not_a_reason"),
            "Source-rule interpretation remains incomplete."
        );
    }

    fn source(start: u64, end: u64) -> EvidenceRef {
        EvidenceRef {
            repo: "example/project".into(),
            path: "src/process.ts".into(),
            byte_start: start,
            byte_end: end,
            commit_sha: "source-revision".into(),
        }
    }

    fn rule() -> GuardedExitEvidence {
        GuardedExitEvidence {
            schema_version: 1,
            local_definitions: None,
            kind: RuleKind::GuardedExit,
            owner_id: "sym:example/project@src/process.ts#run".into(),
            exit_source: source(100, 113),
            source_order: 0,
            conditions: vec![BranchCondition {
                branch_source: source(20, 140),
                expression: SourceExpression {
                    source: source(24, 40),
                    syntax_kind: "binary_expression".into(),
                    display: SanitizedDisplay::from_sanitized("ready !== false".into()),
                    capture: ExpressionCapture::CompleteSyntax,
                    literal: None,
                },
                polarity: BranchPolarity::TruthyBranch,
            }],
            effect: LocalExit::Return {
                value: Some(SourceExpression {
                    source: source(107, 112),
                    syntax_kind: "false".into(),
                    display: SanitizedDisplay::from_sanitized("false".into()),
                    capture: ExpressionCapture::CompleteSyntax,
                    literal: Some(LiteralEvidence::Known {
                        value: KnownLiteral::Boolean(false),
                    }),
                }),
            },
            dependencies: vec![RuleDependency {
                role: DependencyRole::Condition,
                source: source(24, 29),
                resolution: DependencyResolution::Binding {
                    binding_id: "binding:run@10".into(),
                    declaration: source(10, 15),
                },
            }],
            interpretation: Interpretation {
                execution_predicate: InterpretationStatus::NotEstablished,
                consumer_effect: InterpretationStatus::NotEstablished,
                gap_ids: vec!["gap:rule:consumer".into()],
            },
            redactions: vec![],
        }
    }

    #[test]
    fn rule_wire_format_preserves_false_and_withheld_string_as_local_effects() {
        // AC-0122/AC-0124 groundwork: typed storage must not turn false into
        // text or withheld strings into established business rejections.
        let observed = rule();
        let wire = serde_json::to_value(&observed).unwrap();
        assert_eq!(
            wire["effect"]["value"]["literal"]["value"]["kind"],
            "boolean"
        );
        assert_eq!(wire["effect"]["value"]["literal"]["value"]["value"], false);
        assert_eq!(GuardedExitEvidence::from_value(wire).unwrap(), observed);

        let mut withheld = observed;
        let LocalExit::Return { value: Some(value) } = &mut withheld.effect else {
            panic!("fixture has a returned expression");
        };
        value.syntax_kind = "string".into();
        value.display = SanitizedDisplay::from_sanitized("[REDACTED]".into());
        value.capture = ExpressionCapture::Redacted;
        value.literal = Some(LiteralEvidence::Withheld {
            kind: LiteralKind::String,
            reasons: vec![RedactionReason::TokenShaped],
        });
        withheld.redactions.push(SourceRedaction {
            source: value.source.clone(),
            reason: RedactionReason::TokenShaped,
        });
        let wire = serde_json::to_value(&withheld).unwrap();
        assert_eq!(wire["effect"]["kind"], "return");
        assert_eq!(wire["effect"]["value"]["literal"]["kind"], "string");
        assert!(wire["effect"]["value"]["literal"].get("value").is_none());
        assert_eq!(wire["interpretation"]["consumer_effect"], "not_established");
        assert_eq!(GuardedExitEvidence::from_value(wire).unwrap(), withheld);
    }

    #[test]
    fn rule_reader_rejects_unknown_versions_and_established_business_meaning() {
        // AC-0122/AC-0125 groundwork: a reader cannot silently accept a future
        // schema or upgrade a source observation through an unknown wire value.
        let mut wire = serde_json::to_value(rule()).unwrap();
        wire["schema_version"] = 3.into();
        assert_eq!(
            GuardedExitEvidence::from_value(wire),
            Err(RuleValidationError::UnsupportedVersion(3))
        );
        let mut wire = serde_json::to_value(rule()).unwrap();
        wire["interpretation"]["consumer_effect"] = "established_rejection".into();
        assert_eq!(
            GuardedExitEvidence::from_value(wire),
            Err(RuleValidationError::MalformedPayload)
        );
        let mut wire = serde_json::to_value(rule()).unwrap();
        wire["effect"]["raw_source"] = "unexpected source text".into();
        let error = GuardedExitEvidence::from_value(wire).unwrap_err();
        assert_eq!(error.to_string(), "malformed source-rule payload");
    }

    #[test]
    fn guarded_effects_round_trip_and_never_pose_as_exits() {
        // AC-0910: an assignment or call statement is a guarded effect; an
        // effect under the exit kind, or an exit under the effect kind, is
        // malformed.
        let LocalExit::Return { value: Some(value) } = rule().effect else {
            panic!("fixture has a returned expression");
        };
        for effect in [
            LocalExit::Assign {
                target: value.clone(),
                value: value.clone(),
            },
            LocalExit::Call {
                value: value.clone(),
            },
        ] {
            let mut changed = rule();
            changed.kind = RuleKind::GuardedEffect;
            changed.effect = effect;
            let wire = serde_json::to_value(&changed).unwrap();
            assert_eq!(wire["kind"], "guarded_effect");
            assert_eq!(GuardedExitEvidence::from_value(wire).unwrap(), changed);
            let mut posing = changed.clone();
            posing.kind = RuleKind::GuardedExit;
            assert_eq!(
                posing.validate(),
                Err(RuleValidationError::MalformedPayload)
            );
        }
        let mut exit = rule();
        exit.kind = RuleKind::GuardedEffect;
        assert_eq!(exit.validate(), Err(RuleValidationError::MalformedPayload));
    }

    #[test]
    fn local_exit_variants_and_falsy_polarity_survive_without_consumer_meaning() {
        // AC-0122 groundwork: bare return, returned null, and throw are local
        // syntax distinctions; no variant supplies business interpretation.
        let mut bare = rule();
        bare.conditions[0].polarity = BranchPolarity::FalsyBranch;
        bare.effect = LocalExit::Return { value: None };
        let bare_wire = serde_json::to_value(&bare).unwrap();
        assert_eq!(bare_wire["conditions"][0]["polarity"], "falsy_branch");
        assert!(bare_wire["effect"]["value"].is_null());
        assert_eq!(GuardedExitEvidence::from_value(bare_wire).unwrap(), bare);

        let mut thrown = rule();
        let LocalExit::Return { value: Some(value) } = thrown.effect else {
            panic!("fixture has a returned expression");
        };
        thrown.effect = LocalExit::Throw { value };
        let thrown_wire = serde_json::to_value(&thrown).unwrap();
        assert_eq!(thrown_wire["effect"]["kind"], "throw");
        assert_eq!(
            thrown_wire["interpretation"]["consumer_effect"],
            "not_established"
        );
        assert_eq!(
            GuardedExitEvidence::from_value(thrown_wire).unwrap(),
            thrown
        );

        let mut returned_null = rule();
        let LocalExit::Return { value: Some(value) } = &mut returned_null.effect else {
            panic!("fixture has a returned expression");
        };
        value.syntax_kind = "null".into();
        value.display = SanitizedDisplay::from_sanitized("null".into());
        value.literal = Some(LiteralEvidence::Known {
            value: KnownLiteral::Null,
        });
        let null_wire = serde_json::to_value(&returned_null).unwrap();
        assert_eq!(
            null_wire["effect"]["value"]["literal"]["value"]["kind"],
            "null"
        );
        assert_eq!(
            GuardedExitEvidence::from_value(null_wire).unwrap(),
            returned_null
        );
    }

    #[test]
    fn rule_validation_checks_every_nested_source_span() {
        // AC-0122/AC-0124 groundwork: invalid source references must not survive
        // merely because the outer exit itself has a valid span.
        let mut cases = vec![];
        let mut invalid = rule();
        invalid.exit_source = source(113, 100);
        cases.push(invalid);
        let mut invalid = rule();
        invalid.conditions[0].branch_source = source(140, 20);
        cases.push(invalid);
        let mut invalid = rule();
        invalid.conditions[0].expression.source = source(40, 24);
        cases.push(invalid);
        let mut invalid = rule();
        let LocalExit::Return { value: Some(value) } = &mut invalid.effect else {
            panic!("fixture has a returned expression");
        };
        value.source = source(112, 107);
        cases.push(invalid);
        let mut invalid = rule();
        invalid.dependencies[0].source = source(29, 24);
        cases.push(invalid);
        let mut invalid = rule();
        let DependencyResolution::Binding { declaration, .. } =
            &mut invalid.dependencies[0].resolution
        else {
            panic!("fixture has a binding");
        };
        *declaration = source(15, 10);
        cases.push(invalid);
        let mut invalid = rule();
        invalid.redactions.push(SourceRedaction {
            source: source(112, 107),
            reason: RedactionReason::TokenShaped,
        });
        cases.push(invalid);
        let mut empty = rule();
        empty.exit_source = source(100, 100);
        cases.push(empty);
        for invalid in cases {
            assert!(matches!(
                GuardedExitEvidence::from_value(serde_json::to_value(invalid).unwrap()),
                Err(RuleValidationError::InvalidSourceSpan(_))
            ));
        }
    }

    #[test]
    fn rule_source_visitor_covers_all_nested_evidence() {
        // AC-0122 / AC-0125: cached facts must retarget every source reference,
        // including optional return values, throws, bindings and redactions.
        let LocalExit::Return { value: Some(value) } = rule().effect else {
            panic!("fixture has a returned expression");
        };
        for effect in [
            LocalExit::Return {
                value: Some(value.clone()),
            },
            LocalExit::Return { value: None },
            LocalExit::Throw { value },
        ] {
            let has_value = !matches!(&effect, LocalExit::Return { value: None });
            let mut observed = rule();
            observed.effect = effect;
            observed.conditions.push(observed.conditions[0].clone());
            observed.dependencies.extend([
                RuleDependency {
                    role: DependencyRole::ExitValue,
                    source: source(50, 60),
                    resolution: DependencyResolution::Target {
                        node_id: "sym:target".into(),
                    },
                },
                RuleDependency {
                    role: DependencyRole::ControlFlow,
                    source: source(70, 80),
                    resolution: DependencyResolution::Unresolved {
                        gap_id: "gap:dependency".into(),
                    },
                },
            ]);
            observed.redactions.extend([
                SourceRedaction {
                    source: source(90, 95),
                    reason: RedactionReason::RemovedComment,
                },
                SourceRedaction {
                    source: source(96, 99),
                    reason: RedactionReason::CredentialValue,
                },
            ]);
            let original = serde_json::to_value(&observed).unwrap();
            let mut paths = vec![
                "/exit_source",
                "/conditions/0/branch_source",
                "/conditions/0/expression/source",
                "/conditions/1/branch_source",
                "/conditions/1/expression/source",
            ];
            if has_value {
                paths.push("/effect/value/source");
            }
            paths.extend([
                "/dependencies/0/source",
                "/dependencies/0/resolution/declaration",
                "/dependencies/1/source",
                "/dependencies/2/source",
                "/redactions/0/source",
                "/redactions/1/source",
            ]);
            let mut visits = 0;
            observed.visit_sources_mut(|source| {
                source.commit_sha = format!("mapped-{visits}");
                source.byte_start += 1000;
                source.byte_end += 1000;
                visits += 1;
            });
            assert_eq!(visits, paths.len());
            observed.validate().unwrap();
            let actual = serde_json::to_value(&observed).unwrap();
            let mut expected = original;
            for (index, path) in paths.into_iter().enumerate() {
                let reference = expected
                    .pointer_mut(path)
                    .expect("every declared source exists");
                reference["commit_sha"] = serde_json::json!(format!("mapped-{index}"));
                reference["byte_start"] =
                    serde_json::json!(reference["byte_start"].as_u64().unwrap() + 1000);
                reference["byte_end"] =
                    serde_json::json!(reference["byte_end"].as_u64().unwrap() + 1000);
            }
            assert_eq!(
                actual, expected,
                "only the complete source-reference set changes"
            );
        }
    }

    #[test]
    fn withheld_literals_require_a_reason_and_incomplete_capture() {
        // AC-0124 groundwork: a withheld value cannot masquerade as complete
        // captured syntax or lose the explanation for its absence.
        let mut invalid = rule();
        let LocalExit::Return { value: Some(value) } = &mut invalid.effect else {
            panic!("fixture has a returned expression");
        };
        value.literal = Some(LiteralEvidence::Withheld {
            kind: LiteralKind::String,
            reasons: vec![],
        });
        assert_eq!(
            invalid.validate(),
            Err(RuleValidationError::MissingWithholdingReason)
        );
        let LocalExit::Return { value: Some(value) } = &mut invalid.effect else {
            panic!("fixture has a returned expression");
        };
        value.literal = Some(LiteralEvidence::Withheld {
            kind: LiteralKind::String,
            reasons: vec![RedactionReason::CredentialValue],
        });
        assert_eq!(
            invalid.validate(),
            Err(RuleValidationError::InconsistentCapture)
        );
    }
}
