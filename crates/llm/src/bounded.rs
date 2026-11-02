//! SPEC-10 bounded, separately authorized completions. These APIs do not change
//! legacy completion wire formats or consent hashes. Prepared inputs are transient;
//! the host journals consent consumption and invocation start before dispatch.

use crate::{
    AnalysisTier, CompletionAction, CompletionPayload, ConsentGrant, EgressFirewall, EgressPreview,
    LlmProvider, Locality,
};
use reqwest::blocking::{Client, ClientBuilder, RequestBuilder};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

mod request_wire;
mod response_wire;
mod transport_workspace;
pub use transport_workspace::TransportWorkspace;

const RECEIVE_CHUNK_BYTES: usize = 8192;
const MODEL_AND_RESPONSE_ID_BYTES: usize = 256;
mod workspace;
pub use workspace::FirewallWorkspace;

// Shared by validation/serialization and the logical workspace component.
const MAX_PROVIDER_ID_BYTES: usize = 1024;
const MAX_MODEL_BYTES: usize = MODEL_AND_RESPONSE_ID_BYTES;
const MAX_ENDPOINT_BYTES: usize = 4096;
const MAX_ACTION_ID_BYTES: usize = 1024;
const EGRESS_FINGERPRINT_BYTES: u64 = 512 * 1024;
const ORIGINAL_ACTION_OVERHEAD_BYTES: u64 = 4096;

/// Immutable protocol identity, including fixed transport behavior.
pub const PROTOCOL_VERSION: &str = "bounded-json-completion@2";
/// Connection ceiling of reusable bounded clients. A shorter total deadline also
/// shortens connection establishment; clients never get rebuilt for an invocation.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum source spans accepted by the bounded completion transport.
pub const MAX_COMPLETION_SPANS: u32 = 56;

/// Serializable declared limits. Deserialize through [`CompletionLimits`] before
/// trusting caller-supplied values. All byte counts exclude HTTP/TLS framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionLimitValues {
    pub input_bytes: u64,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub output_text_bytes: u64,
    pub max_output_tokens: u32,
    pub request_timeout_ms: u64,
    /// Source-span count is explicit for expanded investigations. Omitting the
    /// legacy allowance preserves existing serialized limits and consent hashes.
    #[serde(
        default = "default_max_spans",
        skip_serializing_if = "legacy_max_spans"
    )]
    pub max_spans: u32,
}

const fn default_max_spans() -> u32 {
    12
}

fn legacy_max_spans(value: &u32) -> bool {
    *value == default_max_spans()
}

impl Default for CompletionLimitValues {
    fn default() -> Self {
        Self {
            input_bytes: 128 * 1024,
            request_bytes: 512 * 1024,
            response_bytes: 256 * 1024,
            output_text_bytes: 32 * 1024,
            max_output_tokens: 2048,
            request_timeout_ms: 180_000,
            max_spans: default_max_spans(),
        }
    }
}

/// Validated positive limits, each independently no greater than SPEC-10's cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(transparent)]
pub struct CompletionLimits(CompletionLimitValues);

impl CompletionLimits {
    pub fn new(values: CompletionLimitValues) -> Result<Self, BoundedCallError> {
        let ceiling = CompletionLimitValues::default();
        let pairs = [
            (values.input_bytes, ceiling.input_bytes),
            (values.request_bytes, ceiling.request_bytes),
            (values.response_bytes, ceiling.response_bytes),
            (values.output_text_bytes, ceiling.output_text_bytes),
            (
                u64::from(values.max_output_tokens),
                u64::from(ceiling.max_output_tokens),
            ),
            (values.request_timeout_ms, ceiling.request_timeout_ms),
            (u64::from(values.max_spans), u64::from(MAX_COMPLETION_SPANS)),
        ];
        if pairs.iter().any(|(value, max)| *value == 0 || value > max) {
            return Err(not_sent(FailureCode::InvalidLimits));
        }
        Ok(Self(values))
    }

    pub fn values(&self) -> CompletionLimitValues {
        self.0
    }
}

impl<'de> Deserialize<'de> for CompletionLimits {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(CompletionLimitValues::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

/// Configured identity, not proof of the model weights that actually executed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderProfile {
    pub protocol_version: String,
    pub provider_id: String,
    pub locality: Locality,
    pub endpoint_id: String,
    pub requested_model: String,
}

impl ProviderProfile {
    fn validate(&self) -> Result<(), BoundedCallError> {
        if self.protocol_version != PROTOCOL_VERSION
            || !identifier(&self.provider_id, MAX_PROVIDER_ID_BYTES)
            || !identifier(&self.requested_model, MAX_MODEL_BYTES)
            || self.endpoint_id.len() > MAX_ENDPOINT_BYTES
        {
            return Err(not_sent(FailureCode::InvalidProfile));
        }
        let url = reqwest::Url::parse(&self.endpoint_id)
            .map_err(|_| not_sent(FailureCode::InvalidProfile))?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.scheme(), "http" | "https")
            || (self.locality == Locality::Local && (url.scheme() != "http" || !loopback(&url)))
            // The only HTTP cloud endpoint is the existing offline fixture seam.
            || (self.locality == Locality::Cloud && url.scheme() == "http" && !loopback(&url))
        {
            return Err(not_sent(FailureCode::InvalidProfile));
        }
        Ok(())
    }
}

/// Exact sizes from the production bounded-wire gates for a complete action.
/// These measurements establish neither source authority nor provider context
/// capacity, egress permission, consent, authorization or dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundedWireMeasurements {
    pub raw_string_bytes: u64,
    pub original_payload_bytes: u64,
    pub redacted_payload_bytes: u64,
    pub original_action_bytes: u64,
    pub egress_fingerprint_bytes: u64,
}

/// Measure complete actual representations through the same ordered serializers
/// and redactor used by the firewall. No provider or authorization is consulted.
/// Provider HTTP request encoding and tokenizer/context fit are not measured.
/// Unknown/prospective source contributions must be established by the caller;
/// substituting an empty source cannot certify a future nonempty action.
pub fn measure_bounded_action(
    profile: &ProviderProfile,
    action: &CompletionAction,
    limits: &CompletionLimits,
) -> Result<BoundedWireMeasurements, BoundedCallError> {
    profile.validate()?;
    prepare_wire(profile, action, limits).map(|prepared| prepared.measurements)
}

/// Pure inspection of the exact bounded redaction/serialization path. This is
/// neither policy authorization nor a provider preparation/dispatch capability.
///
/// ```compile_fail
/// fn promote(value: llm::bounded::BoundedWireInspection) -> llm::bounded::PreparedBoundedCompletion {
///     value.into()
/// }
/// ```
pub struct BoundedWireInspection {
    payload: CompletionPayload,
    measurements: BoundedWireMeasurements,
}
impl BoundedWireInspection {
    /// Exact transient redacted representation; it is not an execution receipt.
    pub fn payload(&self) -> &CompletionPayload {
        &self.payload
    }
    pub fn measurements(&self) -> BoundedWireMeasurements {
        self.measurements
    }
}

/// Inspect fixed protocol fields without constructing a provider or a sealable
/// PreparedBoundedCompletion. Profile/redactor identity and all wire gates are
/// identical to the real firewall's prepare_wire path.
pub fn inspect_bounded_action(
    profile: &ProviderProfile,
    action: &CompletionAction,
    limits: &CompletionLimits,
) -> Result<BoundedWireInspection, BoundedCallError> {
    profile.validate()?;
    let wire = prepare_wire(profile, action, limits)?;
    Ok(BoundedWireInspection {
        payload: wire.payload,
        measurements: wire.measurements,
    })
}

/// Conservative wire bounds for replacing one empty source-text field. Every
/// other field must already be the complete prospective representation (including
/// continuation feedback). This proves only the five bounded-wire gates, never
/// source authority, prompt closure, provider framing or permission to read.
/// The source text is a direct JSON string in each measured representation.
pub fn bound_unread_source_action(
    profile: &ProviderProfile,
    skeleton: &CompletionAction,
    limits: &CompletionLimits,
    span_index: usize,
    source_bytes: usize,
) -> Result<BoundedWireMeasurements, BoundedCallError> {
    let span = skeleton
        .payload
        .spans
        .get(span_index)
        .ok_or_else(|| not_sent(FailureCode::InvalidInput))?;
    if !span.text.is_empty() {
        return Err(not_sent(FailureCode::InvalidInput));
    }
    let mut sizes = measure_bounded_action(profile, skeleton, limits)?;
    let escaped = core_redact::direct_source_json_content_bound(source_bytes)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| not_sent(FailureCode::InputTooLarge))?;
    let raw = u64::try_from(source_bytes).map_err(|_| not_sent(FailureCode::InputTooLarge))?;
    let add = |base: u64, delta: u64, ceiling: u64| {
        base.checked_add(delta)
            .filter(|value| *value <= ceiling)
            .ok_or_else(|| not_sent(FailureCode::InputTooLarge))
    };
    sizes.raw_string_bytes = add(sizes.raw_string_bytes, raw, limits.0.input_bytes)?;
    sizes.original_payload_bytes =
        add(sizes.original_payload_bytes, escaped, limits.0.input_bytes)?;
    sizes.redacted_payload_bytes =
        add(sizes.redacted_payload_bytes, escaped, limits.0.input_bytes)?;
    sizes.original_action_bytes = add(
        sizes.original_action_bytes,
        escaped,
        limits.0.input_bytes + ORIGINAL_ACTION_OVERHEAD_BYTES,
    )?;
    sizes.egress_fingerprint_bytes = add(
        sizes.egress_fingerprint_bytes,
        escaped,
        EGRESS_FINGERPRINT_BYTES,
    )?;
    Ok(sizes)
}

struct PreparedWire {
    payload: CompletionPayload,
    redaction_count: usize,
    original_action_hash: String,
    payload_hash: String,
    measurements: BoundedWireMeasurements,
}

/// Exact preview; this includes source text and must stay transient. The embedded
/// preview reuses the consent renderer and grant shape, with a new hash domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoundedEgressPreview {
    pub egress: EgressPreview,
    pub profile: ProviderProfile,
    pub limits: CompletionLimits,
}

/// Only the firewall constructs this validated, redacted input. No deserializer.
pub struct PreparedBoundedCompletion {
    preview: BoundedEgressPreview,
    original_action_hash: String,
}

impl std::fmt::Debug for PreparedBoundedCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedBoundedCompletion")
            .field("payload_hash", &self.preview.egress.payload_hash)
            .finish_non_exhaustive()
    }
}

impl PreparedBoundedCompletion {
    /// Bind a caller's validated acquisition to this firewall preparation even
    /// when redaction changed the payload. This transient hash is never an
    /// egress identity or a replacement for exact-preview consent.
    pub fn matches_action(&self, action: &CompletionAction) -> bool {
        original_action_hash(action, self.preview.limits.0.input_bytes)
            .is_ok_and(|hash| hash == self.original_action_hash)
    }

    pub fn preview(&self) -> &BoundedEgressPreview {
        &self.preview
    }
    pub fn profile(&self) -> &ProviderProfile {
        &self.preview.profile
    }
    pub fn limits(&self) -> &CompletionLimits {
        &self.preview.limits
    }
}

/// Firewall-authorized transient input. Durable one-use consent consumption is
/// enforced by the coordinator, not by this in-memory provider capability.
pub struct AuthorizedBoundedCompletion {
    preview: BoundedEgressPreview,
}

impl std::fmt::Debug for AuthorizedBoundedCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorizedBoundedCompletion")
            .field("payload_hash", &self.preview.egress.payload_hash)
            .finish_non_exhaustive()
    }
}

impl AuthorizedBoundedCompletion {
    pub fn payload(&self) -> &CompletionPayload {
        &self.preview.egress.payload
    }
    pub fn profile(&self) -> &ProviderProfile {
        &self.preview.profile
    }
    pub fn limits(&self) -> &CompletionLimits {
        &self.preview.limits
    }
    pub fn payload_hash(&self) -> &str {
        &self.preview.egress.payload_hash
    }
    pub fn action_id(&self) -> &str {
        &self.preview.egress.action_id
    }
    pub fn tier(&self) -> AnalysisTier {
        self.preview.egress.tier
    }
}

/// Host checks are brief and source-free. They may inspect durable ownership;
/// no lock is held while waiting for HTTP. Blocking transport is cooperative.
pub struct CompletionControl<'a> {
    pub deadline: Instant,
    pub check: &'a dyn Fn() -> CallDirective,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallDirective {
    Continue,
    Cancelled,
    OwnershipLost,
}

/// Transport evidence only; it does not establish remote cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationOutcome {
    NotDispatched,
    ResponseReceived,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    Unsupported,
    InvalidLimits,
    InvalidInput,
    InvalidProfile,
    ProfileChanged,
    EgressDenied,
    ConsentRequired,
    ConsentMismatch,
    InputTooLarge,
    RequestTooLarge,
    ResponseTooLarge,
    OutputTooLarge,
    Cancelled,
    OwnershipLost,
    DeadlineExceeded,
    Transport,
    HttpStatus,
    UnsupportedEncoding,
    InvalidResponse,
    IncompleteResponse,
    Refused,
    EmptyCompletion,
}

/// Fixed diagnostics never retain transport URLs, body snippets, parser errors,
/// credentials, source text or provider-controlled refusal explanations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("bounded provider call: {code:?} ({outcome:?})")]
pub struct BoundedCallError {
    pub code: FailureCode,
    pub outcome: InvocationOutcome,
}

impl BoundedCallError {
    pub fn unsupported() -> Self {
        not_sent(FailureCode::Unsupported)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportedUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
}

/// Text is transient until the caller admits the strict action and source-replay
/// policy. Usage is provider-reported, not verified billing or token accounting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundedCompletion {
    pub text: String,
    pub requested_model: String,
    pub response_model: Option<String>,
    pub provider_request_id: Option<String>,
    pub usage: Option<ReportedUsage>,
    pub request_bytes: u64,
    pub response_bytes: u64,
}

impl EgressFirewall {
    /// Validate/redact and fingerprint before preview; never invokes a provider.
    pub fn prepare_bounded(
        &self,
        provider: &dyn LlmProvider,
        action: &CompletionAction,
        limits: &CompletionLimits,
    ) -> Result<PreparedBoundedCompletion, BoundedCallError> {
        let profile = provider.bounded_profile()?;
        validate_provider(provider, &profile)?;
        check_policy(self, &profile, action.tier)?;
        if action.payload.response_schema.is_some() && !provider.supports_bounded_response_schema()
        {
            return Err(BoundedCallError::unsupported());
        }
        let wire = prepare_wire(&profile, action, limits)?;
        Ok(PreparedBoundedCompletion {
            original_action_hash: wire.original_action_hash,
            preview: BoundedEgressPreview {
                egress: EgressPreview {
                    provider_id: profile.provider_id.clone(),
                    locality: profile.locality,
                    tier: action.tier,
                    action_id: action.action_id.clone(),
                    payload: wire.payload,
                    payload_hash: wire.payload_hash,
                    redaction_count: wire.redaction_count,
                },
                profile,
                limits: *limits,
            },
        })
    }

    /// Recheck current policy and exact identity after a consent wait. The host
    /// atomically consumes approval/journals the call after this returns.
    pub fn authorize_bounded(
        &self,
        provider: &dyn LlmProvider,
        prepared: &PreparedBoundedCompletion,
        consent: Option<&ConsentGrant>,
    ) -> Result<AuthorizedBoundedCompletion, BoundedCallError> {
        let profile = provider.bounded_profile()?;
        validate_provider(provider, &profile)?;
        if profile != prepared.preview.profile {
            return Err(not_sent(FailureCode::ProfileChanged));
        }
        if prepared.preview.egress.payload.response_schema.is_some()
            && !provider.supports_bounded_response_schema()
        {
            return Err(BoundedCallError::unsupported());
        }
        check_policy(self, &profile, prepared.preview.egress.tier)?;
        if profile.locality == Locality::Cloud {
            let grant = consent.ok_or_else(|| not_sent(FailureCode::ConsentRequired))?;
            if !grant.matches(&prepared.preview.egress) {
                return Err(not_sent(FailureCode::ConsentMismatch));
            }
        }
        Ok(AuthorizedBoundedCompletion {
            preview: prepared.preview.clone(),
        })
    }
}

// Borrow validated raw JSON without a Value tree or schema rewrite.
fn response_schema(
    payload: &CompletionPayload,
) -> Result<Option<&serde_json::value::RawValue>, BoundedCallError> {
    payload
        .response_schema
        .as_deref()
        .map(|schema| {
            if !schema.trim_start().starts_with('{') {
                return Err(not_sent(FailureCode::InvalidInput));
            }
            serde_json::from_str(schema).map_err(|_| not_sent(FailureCode::InvalidInput))
        })
        .transpose()
}

fn prepare_wire(
    profile: &ProviderProfile,
    action: &CompletionAction,
    limits: &CompletionLimits,
) -> Result<PreparedWire, BoundedCallError> {
    if !identifier(&action.action_id, MAX_ACTION_ID_BYTES)
        || action.payload.spans.len() > limits.0.max_spans as usize
    {
        return Err(not_sent(FailureCode::InvalidInput));
    }
    let mut ids = std::collections::BTreeSet::new();
    for span in &action.payload.spans {
        if !identifier(&span.id, 1024) || span.byte_start >= span.byte_end || !ids.insert(&span.id)
        {
            return Err(not_sent(FailureCode::InvalidInput));
        }
    }
    let strings = [&action.payload.system, &action.payload.prompt]
        .into_iter()
        .chain(action.payload.response_schema.iter())
        .chain(action.payload.spans.iter().flat_map(|span| {
            [
                &span.id,
                &span.repo,
                &span.path,
                &span.commit_sha,
                &span.text,
            ]
        }));
    let mut input_bytes = 0_u64;
    for value in strings {
        input_bytes = input_bytes
            .checked_add(value.len() as u64)
            .filter(|total| *total <= limits.0.input_bytes)
            .ok_or_else(|| not_sent(FailureCode::InputTooLarge))?;
    }
    // This walk aborts before an unbounded buffer, regex input or clone can
    // be allocated. Logical JSON includes all metadata, not just span text.
    let original_payload_bytes = encode(
        &action.payload,
        limits.0.input_bytes,
        FailureCode::InputTooLarge,
    )?
    .len() as u64;
    if let Some(schema) = &action.payload.response_schema {
        // Reject escaped text instead of pretending raw-text redaction inspects
        // decoded schema strings. Never rewrite accepted schema semantics.
        if schema.contains('\\') || crate::redact_text(schema).1 != 0 {
            return Err(not_sent(FailureCode::InvalidInput));
        }
    }
    response_schema(&action.payload)?;
    let (mut payload, mut redaction_count) = crate::redact_payload(&action.payload);
    // Legacy redaction is unchanged. The new protocol covers metadata too.
    for span in &mut payload.spans {
        for value in [
            &mut span.id,
            &mut span.repo,
            &mut span.path,
            &mut span.commit_sha,
        ] {
            let (redacted, count) = crate::redact_text(value);
            *value = redacted;
            redaction_count += count;
        }
    }
    let mut redacted_ids = std::collections::BTreeSet::new();
    if payload
        .spans
        .iter()
        .any(|span| !redacted_ids.insert(&span.id))
    {
        return Err(not_sent(FailureCode::InvalidInput));
    }
    let redacted_payload_bytes =
        encode(&payload, limits.0.input_bytes, FailureCode::InputTooLarge)?.len() as u64;
    let hash_input = encode(
        &(
            "cartograph-bounded-egress-v1",
            &profile,
            limits,
            action.tier,
            &action.action_id,
            &payload,
        ),
        EGRESS_FINGERPRINT_BYTES,
        FailureCode::InputTooLarge,
    )?;
    let payload_hash = blake3::hash(&hash_input).to_hex().to_string();
    // Keep the original-action binding last, as in the existing firewall.
    let (original_action_hash, original_action_bytes) =
        original_action_wire(action, limits.0.input_bytes)?;
    Ok(PreparedWire {
        payload,
        redaction_count,
        original_action_hash,
        payload_hash,
        measurements: BoundedWireMeasurements {
            raw_string_bytes: input_bytes,
            original_payload_bytes,
            redacted_payload_bytes,
            original_action_bytes,
            egress_fingerprint_bytes: hash_input.len() as u64,
        },
    })
}

fn original_action_hash(
    action: &CompletionAction,
    payload_limit: u64,
) -> Result<String, BoundedCallError> {
    original_action_wire(action, payload_limit).map(|(hash, _)| hash)
}

fn original_action_wire(
    action: &CompletionAction,
    payload_limit: u64,
) -> Result<(String, u64), BoundedCallError> {
    // Payload has its own existing bound; the envelope adds a bounded action ID.
    encode(&action.payload, payload_limit, FailureCode::InputTooLarge)?;
    let bytes = encode(
        &("cartograph-original-bounded-action-v1", action),
        payload_limit + ORIGINAL_ACTION_OVERHEAD_BYTES,
        FailureCode::InputTooLarge,
    )?;
    Ok((
        blake3::hash(&bytes).to_hex().to_string(),
        bytes.len() as u64,
    ))
}

fn validate_provider(
    provider: &dyn LlmProvider,
    profile: &ProviderProfile,
) -> Result<(), BoundedCallError> {
    profile.validate()?;
    if !provider.capabilities().chat {
        return Err(BoundedCallError::unsupported());
    }
    if profile.provider_id != provider.id() || profile.locality != provider.locality() {
        return Err(not_sent(FailureCode::InvalidProfile));
    }
    Ok(())
}

fn check_policy(
    firewall: &EgressFirewall,
    profile: &ProviderProfile,
    tier: AnalysisTier,
) -> Result<(), BoundedCallError> {
    if profile.locality == Locality::Cloud && !firewall.policy.cloud_allowed(tier) {
        Err(not_sent(FailureCode::EgressDenied))
    } else {
        Ok(())
    }
}

fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn loopback(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

fn not_sent(code: FailureCode) -> BoundedCallError {
    BoundedCallError {
        code,
        outcome: InvocationOutcome::NotDispatched,
    }
}
fn rejected(code: FailureCode) -> BoundedCallError {
    BoundedCallError {
        code,
        outcome: InvocationOutcome::ResponseReceived,
    }
}
fn unknown(code: FailureCode) -> BoundedCallError {
    BoundedCallError {
        code,
        outcome: InvocationOutcome::Unknown,
    }
}

struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}
impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("bounded serialization limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode(
    value: &impl Serialize,
    limit: u64,
    code: FailureCode,
) -> Result<Vec<u8>, BoundedCallError> {
    let mut writer = LimitedWriter {
        bytes: Vec::new(),
        limit: usize::try_from(limit).map_err(|_| not_sent(FailureCode::InvalidLimits))?,
        exceeded: false,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| {
        not_sent(if writer.exceeded {
            code
        } else {
            FailureCode::InvalidInput
        })
    })?;
    Ok(writer.bytes)
}

/// Construct once with the provider and retain at application/coordinator scope.
pub(super) fn build_client(builder: ClientBuilder, local: bool) -> Result<Client, reqwest::Error> {
    let builder = builder
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_gzip()
        .no_brotli()
        .no_zstd()
        .no_deflate()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(Duration::from_secs(180));
    let builder = if local { builder.no_proxy() } else { builder };
    builder.build()
}

pub(super) fn ollama_profile(
    id: &str,
    base: &reqwest::Url,
    model: &str,
) -> Result<ProviderProfile, BoundedCallError> {
    let endpoint = base
        .join("api/chat")
        .map_err(|_| not_sent(FailureCode::InvalidProfile))?;
    let profile = ProviderProfile {
        protocol_version: PROTOCOL_VERSION.into(),
        provider_id: id.into(),
        locality: Locality::Local,
        endpoint_id: endpoint.to_string(),
        requested_model: model.into(),
    };
    profile.validate()?;
    Ok(profile)
}

pub(super) fn openai_compatible_profile(
    id: &str,
    base: &reqwest::Url,
    model: &str,
) -> Result<ProviderProfile, BoundedCallError> {
    let endpoint = base
        .join("v1/chat/completions")
        .map_err(|_| not_sent(FailureCode::InvalidProfile))?;
    let profile = ProviderProfile {
        protocol_version: PROTOCOL_VERSION.into(),
        provider_id: id.into(),
        locality: Locality::Local,
        endpoint_id: endpoint.to_string(),
        requested_model: model.into(),
    };
    profile.validate()?;
    Ok(profile)
}

fn check_control(control: &CompletionControl<'_>, sent: bool) -> Result<(), BoundedCallError> {
    let code = match (control.check)() {
        CallDirective::Cancelled => Some(FailureCode::Cancelled),
        CallDirective::OwnershipLost => Some(FailureCode::OwnershipLost),
        CallDirective::Continue if Instant::now() >= control.deadline => {
            Some(FailureCode::DeadlineExceeded)
        }
        CallDirective::Continue => None,
    };
    match code {
        Some(code) => Err(if sent { unknown(code) } else { not_sent(code) }),
        None => Ok(()),
    }
}

fn check_request(
    profile: &ProviderProfile,
    request: &AuthorizedBoundedCompletion,
    control: &CompletionControl<'_>,
) -> Result<(), BoundedCallError> {
    profile.validate()?;
    if profile != request.profile() {
        return Err(not_sent(FailureCode::ProfileChanged));
    }
    check_control(control, false)
}

fn user_message(request: &AuthorizedBoundedCompletion) -> Result<String, BoundedCallError> {
    let spans = encode(
        &request.payload().spans,
        request.limits().0.input_bytes,
        FailureCode::InputTooLarge,
    )?;
    let spans = String::from_utf8(spans).map_err(|_| not_sent(FailureCode::InvalidInput))?;
    Ok(format!(
        "{}\n\nEvidence spans (JSON):\n{}",
        request.payload().prompt,
        spans
    ))
}

/// Send one bounded call. When the host installed a debug log (#1624), a
/// bounded excerpt of the exact request body is queued before the final
/// control check and of the exact response body (or the failure code) when
/// the call ends. Copies are made only inside the debug log's own fixed
/// pool, and queuing never blocks on the log writer.
fn acquire(
    builder: RequestBuilder,
    body: Vec<u8>,
    request: &AuthorizedBoundedCompletion,
    control: &CompletionControl<'_>,
) -> Result<(Vec<u8>, u64), BoundedCallError> {
    let mut call = None;
    let result = acquire_unlogged(builder, body, request, control, &mut call);
    if let Some(call) = call {
        call.finish(
            result.as_ref().ok().map(|(bytes, _)| bytes.as_slice()),
            result
                .as_ref()
                .err()
                .map(|error| format!("{:?}", error.code)),
        );
    }
    result
}

fn acquire_unlogged(
    builder: RequestBuilder,
    body: Vec<u8>,
    request: &AuthorizedBoundedCompletion,
    control: &CompletionControl<'_>,
    call: &mut Option<crate::debug_log::Call>,
) -> Result<(Vec<u8>, u64), BoundedCallError> {
    check_control(control, false)?;
    let remaining = control.deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(not_sent(FailureCode::DeadlineExceeded));
    }
    let timeout = Duration::from_millis(request.limits().0.request_timeout_ms).min(remaining);
    let call_deadline = Instant::now() + timeout;
    let request_bytes = body.len() as u64;
    let (client, built) = builder
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .timeout(timeout)
        .body(body)
        .build_split();
    let mut built = built.map_err(|_| not_sent(FailureCode::InvalidInput))?;
    if crate::debug_log::enabled() {
        let sent = built
            .body()
            .and_then(reqwest::blocking::Body::as_bytes)
            .unwrap_or_default();
        *call = Some(crate::debug_log::begin(
            "bounded",
            built.url().as_str(),
            &[sent],
        ));
    }
    // Checked after the debug entry is queued, so neither cancellation nor
    // the deadline can be missed while it was recorded.
    check_control(control, false)?;
    *built.timeout_mut() =
        Some(timeout.min(control.deadline.saturating_duration_since(Instant::now())));
    let mut response = client.execute(built).map_err(|error| {
        unknown(if error.is_timeout() {
            FailureCode::DeadlineExceeded
        } else {
            FailureCode::Transport
        })
    })?;
    if !response.status().is_success() {
        return Err(rejected(FailureCode::HttpStatus));
    }
    if response
        .headers()
        .get_all(reqwest::header::CONTENT_ENCODING)
        .iter()
        .any(|value| value.as_bytes() != b"identity")
    {
        return Err(rejected(FailureCode::UnsupportedEncoding));
    }
    let cap = request.limits().0.response_bytes as usize;
    if response
        .content_length()
        .is_some_and(|len| len > cap as u64)
    {
        return Err(rejected(FailureCode::ResponseTooLarge));
    }
    let mut body = Vec::new();
    let mut chunk = [0_u8; RECEIVE_CHUNK_BYTES];
    loop {
        // Always probe one byte beyond the cap; Content-Length is not authority.
        let read_limit = chunk.len().min(cap.saturating_sub(body.len()) + 1);
        let count = response.read(&mut chunk[..read_limit]).map_err(|error| {
            let timed_out = matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) || error
                .get_ref()
                .and_then(|e| e.downcast_ref::<reqwest::Error>())
                .is_some_and(reqwest::Error::is_timeout)
                || Instant::now() >= call_deadline;
            unknown(if timed_out {
                FailureCode::DeadlineExceeded
            } else {
                FailureCode::Transport
            })
        })?;
        // A complete response wins a simultaneous cancellation. Admission and
        // persistence still run, but the host may never execute its next tool.
        if count == 0 {
            break;
        }
        if count > cap.saturating_sub(body.len()) {
            return Err(rejected(FailureCode::ResponseTooLarge));
        }
        body.extend_from_slice(&chunk[..count]);
        // Once issued, finish this bounded response or its total timeout. A
        // cancellation observed between the last data chunk and EOF must not
        // erase an otherwise complete result. The coordinator stops further work.
    }
    Ok((body, request_bytes))
}

fn finish(
    text: String,
    model: Option<String>,
    id: Option<String>,
    usage: Option<ReportedUsage>,
    request: &AuthorizedBoundedCompletion,
    request_bytes: u64,
    response_bytes: usize,
) -> Result<BoundedCompletion, BoundedCallError> {
    if text.len() as u64 > request.limits().0.output_text_bytes {
        return Err(rejected(FailureCode::OutputTooLarge));
    }
    if text.trim().is_empty() {
        return Err(rejected(FailureCode::EmptyCompletion));
    }
    if model
        .as_ref()
        .is_some_and(|model| !identifier(model, MODEL_AND_RESPONSE_ID_BYTES))
        || id
            .as_ref()
            .is_some_and(|id| !identifier(id, MODEL_AND_RESPONSE_ID_BYTES))
        || usage.as_ref().is_some_and(|usage| {
            [
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_read_input_tokens,
                usage.cache_creation_input_tokens,
            ]
            .into_iter()
            .flatten()
            .any(|tokens| tokens > 1_000_000_000)
                || usage
                    .output_tokens
                    .is_some_and(|tokens| tokens > u64::from(request.limits().0.max_output_tokens))
        })
    {
        return Err(rejected(FailureCode::InvalidResponse));
    }
    Ok(BoundedCompletion {
        text,
        requested_model: request.profile().requested_model.clone(),
        response_model: model,
        provider_request_id: id,
        usage,
        request_bytes,
        response_bytes: response_bytes as u64,
    })
}

// Check structural work before serde allocates response containers, including
// ignored provider fields. Escaped model action JSON is a string, not containers.
// The typed decoder remains authoritative for JSON syntax.
fn preflight_response(bytes: &[u8]) -> Result<(), BoundedCallError> {
    let mut quoted = false;
    let mut escaped = false;
    let mut depth = 0_usize;
    let mut containers = 0_usize;
    for byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => {
                depth += 1;
                containers += 1;
                if depth > 64 || containers > 4096 {
                    return Err(rejected(FailureCode::InvalidResponse));
                }
            }
            b'}' | b']' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| rejected(FailureCode::InvalidResponse))?;
            }
            _ => (),
        }
    }
    if quoted || depth != 0 {
        return Err(rejected(FailureCode::InvalidResponse));
    }
    Ok(())
}

pub(super) fn complete_ollama(
    client: &Client,
    profile: &ProviderProfile,
    request: &AuthorizedBoundedCompletion,
    control: &CompletionControl<'_>,
) -> Result<BoundedCompletion, BoundedCallError> {
    check_request(profile, request, control)?;
    let user = user_message(request)?;
    let body = encode(
        &request_wire::ollama(
            &profile.requested_model,
            &request.payload().system,
            &user,
            request.limits().0.max_output_tokens,
            response_schema(request.payload())?,
        ),
        request.limits().0.request_bytes,
        FailureCode::RequestTooLarge,
    )?;
    let (bytes, request_bytes) =
        acquire(client.post(&profile.endpoint_id), body, request, control)?;
    preflight_response(&bytes)?;
    let response: response_wire::OllamaResponse =
        serde_json::from_slice(&bytes).map_err(|_| rejected(FailureCode::InvalidResponse))?;
    if !response.done || response.done_reason != "stop" {
        return Err(rejected(FailureCode::IncompleteResponse));
    }
    if response.message.role != "assistant"
        || !response.message.tool_calls.is_empty()
        || !response.message.images.is_empty()
    {
        return Err(rejected(FailureCode::InvalidResponse));
    }
    let usage = (response.prompt_eval_count.is_some() || response.eval_count.is_some()).then_some(
        ReportedUsage {
            input_tokens: response.prompt_eval_count,
            output_tokens: response.eval_count,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        },
    );
    finish(
        response.message.content,
        response.model,
        None,
        usage,
        request,
        request_bytes,
        bytes.len(),
    )
}

/// Admitted completion text, response model, response id and usage.
pub(super) type Admitted = (
    String,
    Option<String>,
    Option<String>,
    Option<ReportedUsage>,
);

/// An OpenAI-compatible completion's text, model, id and usage, admitted
/// only as one assistant choice that stopped normally without tool calls.
/// Shared by the bounded and legacy completion paths.
pub(super) fn openai_compatible_envelope(bytes: &[u8]) -> Result<Admitted, BoundedCallError> {
    preflight_response(bytes)?;
    let response: response_wire::OpenAiResponse =
        serde_json::from_slice(bytes).map_err(|_| rejected(FailureCode::InvalidResponse))?;
    if response.object != "chat.completion" || response.choices.len() != 1 {
        return Err(rejected(FailureCode::InvalidResponse));
    }
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| rejected(FailureCode::InvalidResponse))?;
    let refused = choice.finish_reason.as_deref() == Some("content_filter")
        || choice
            .message
            .refusal
            .as_ref()
            .is_some_and(|refusal| !refusal.is_empty());
    if refused {
        return Err(rejected(FailureCode::Refused));
    }
    if choice.finish_reason.as_deref() != Some("stop") {
        return Err(rejected(FailureCode::IncompleteResponse));
    }
    // A separate `reasoning` field is ignored but counted in the body cap.
    if choice.message.role != "assistant"
        || choice
            .message
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty())
        || choice.message.function_call.is_some()
    {
        return Err(rejected(FailureCode::InvalidResponse));
    }
    let usage = response.usage.map(|usage| ReportedUsage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
    });
    Ok((
        choice.message.content.unwrap_or_default(),
        response.model,
        response.id,
        usage,
    ))
}

pub(super) fn complete_openai_compatible(
    client: &Client,
    profile: &ProviderProfile,
    request: &AuthorizedBoundedCompletion,
    control: &CompletionControl<'_>,
) -> Result<BoundedCompletion, BoundedCallError> {
    check_request(profile, request, control)?;
    if request.payload().response_schema.is_some() {
        return Err(BoundedCallError::unsupported());
    }
    let user = user_message(request)?;
    let body = encode(
        &request_wire::openai_compatible(
            &profile.requested_model,
            &request.payload().system,
            &user,
            request.limits().0.max_output_tokens,
        ),
        request.limits().0.request_bytes,
        FailureCode::RequestTooLarge,
    )?;
    let (bytes, request_bytes) =
        acquire(client.post(&profile.endpoint_id), body, request, control)?;
    let (text, model, id, usage) = openai_compatible_envelope(&bytes)?;
    finish(text, model, id, usage, request, request_bytes, bytes.len())
}

pub(super) fn complete_anthropic(
    client: &Client,
    api_key: &str,
    profile: &ProviderProfile,
    request: &AuthorizedBoundedCompletion,
    control: &CompletionControl<'_>,
) -> Result<BoundedCompletion, BoundedCallError> {
    check_request(profile, request, control)?;
    if request.payload().response_schema.is_some() {
        return Err(BoundedCallError::unsupported());
    }
    if api_key.is_empty()
        || api_key.len() > 4096
        || reqwest::header::HeaderValue::from_str(api_key).is_err()
    {
        return Err(not_sent(FailureCode::InvalidProfile));
    }
    let user = user_message(request)?;
    let body = encode(
        &request_wire::anthropic(
            &profile.requested_model,
            &request.payload().system,
            &user,
            request.limits().0.max_output_tokens,
        ),
        request.limits().0.request_bytes,
        FailureCode::RequestTooLarge,
    )?;
    let builder = client
        .post(&profile.endpoint_id)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01");
    let (bytes, request_bytes) = acquire(builder, body, request, control)?;
    preflight_response(&bytes)?;
    let response: response_wire::AnthropicResponse =
        serde_json::from_slice(&bytes).map_err(|_| rejected(FailureCode::InvalidResponse))?;
    if response.stop_reason == "refusal"
        || response
            .stop_details
            .is_some_and(|details| details.kind.as_deref() == Some("refusal"))
    {
        return Err(rejected(FailureCode::Refused));
    }
    if response.stop_reason != "end_turn" {
        return Err(rejected(FailureCode::IncompleteResponse));
    }
    if response.kind != "message" || response.role != "assistant" || response.content.len() > 32 {
        return Err(rejected(FailureCode::InvalidResponse));
    }
    let mut text = String::new();
    for block in response.content {
        match block.kind.as_str() {
            "text" => {
                let part = block
                    .text
                    .ok_or_else(|| rejected(FailureCode::InvalidResponse))?;
                if part.len() as u64
                    > request
                        .limits()
                        .0
                        .output_text_bytes
                        .saturating_sub(text.len() as u64)
                {
                    return Err(rejected(FailureCode::OutputTooLarge));
                }
                text.push_str(&part);
            }
            // Ignored private thinking still counted in the complete body cap.
            "thinking" | "redacted_thinking" => (),
            _ => return Err(rejected(FailureCode::InvalidResponse)),
        }
    }
    let usage = response.usage.map(|usage| ReportedUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
    });
    finish(
        text,
        response.model,
        response.id,
        usage,
        request,
        request_bytes,
        bytes.len(),
    )
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod wire_tests;
