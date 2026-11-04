//! Aggregate accounting algebra for the durable host transaction. These values
//! are not execution grants; the host must authenticate and persist each update.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticAuditAccount {
    schema_version: u32,
    limits_hash: String,
    analyst: InvestigationUsage,
    auditor: InvestigationUsage,
    audit_started: bool,
    terminal: bool,
}

impl SemanticAuditAccount {
    /// Reserve one audit invocation under the original immutable task limits.
    pub fn new(limits: &InvestigationLimits) -> Result<Self, InvestigationError> {
        let account = Self {
            schema_version: 1,
            limits_hash: limits_hash(limits)?,
            analyst: InvestigationUsage::default(),
            auditor: InvestigationUsage::default(),
            audit_started: false,
            terminal: false,
        };
        account.validate(limits)?;
        Ok(account)
    }

    /// Cumulative task usage, not a caller-supplied delta. Regressions fail.
    /// Returns a replacement so a failed admission cannot partly mutate state.
    pub fn analyst_usage(
        &self,
        usage: &InvestigationUsage,
        limits: &InvestigationLimits,
    ) -> Result<Self, InvestigationError> {
        self.validate(limits)?;
        if self.audit_started || self.terminal {
            return Err(InvestigationError::InvalidInput);
        }
        monotonic(&self.analyst, usage)?;
        let mut next = self.clone();
        next.analyst = usage.clone();
        next.validate(limits)?;
        Ok(next)
    }

    /// Consume the one reserved call exactly once, before provider dispatch.
    /// Unknown provider outcomes retain this charge; there is no retry method.
    pub fn begin_audit(&self, limits: &InvestigationLimits) -> Result<Self, InvestigationError> {
        self.validate(limits)?;
        if self.audit_started || self.terminal {
            return Err(InvestigationError::InvalidInput);
        }
        let mut next = self.clone();
        next.audit_started = true;
        next.auditor.model_invocations = 1;
        next.auditor.generated_token_reservations = limits.generated_tokens_per_invocation;
        next.validate(limits)?;
        Ok(next)
    }

    /// Includes any acquisition/validation costs attributed to the audit stage.
    pub fn auditor_usage(
        &self,
        usage: &InvestigationUsage,
        limits: &InvestigationLimits,
    ) -> Result<Self, InvestigationError> {
        self.validate(limits)?;
        if self.terminal
            || !self.audit_started
            || usage.model_invocations != 1
            || usage.generated_token_reservations != limits.generated_tokens_per_invocation
        {
            return Err(InvestigationError::InvalidInput);
        }
        monotonic(&self.auditor, usage)?;
        let mut next = self.clone();
        next.auditor = usage.clone();
        next.validate(limits)?;
        Ok(next)
    }

    /// Frozen analyst-stage counters after audit dispatch. Reading these is not
    /// evidence that the host persisted the corresponding invocation journal.
    pub fn analyst_counters(&self) -> &InvestigationUsage {
        &self.analyst
    }

    pub fn is_terminal(&self) -> bool {
        self.terminal
    }

    pub fn audit_started(&self) -> bool {
        self.audit_started
    }

    pub fn usage(&self) -> Result<InvestigationUsage, InvestigationError> {
        sum(&self.analyst, &self.auditor)
    }

    /// Retain a measured terminal analyst response even when its clock sample
    /// crosses the deadline. This permits no further calls and grants no budget.
    pub fn stop_analyst(
        &self,
        usage: &InvestigationUsage,
        limits: &InvestigationLimits,
    ) -> Result<Self, InvestigationError> {
        self.validate(limits)?;
        if self.audit_started || self.terminal {
            return Err(InvestigationError::InvalidInput);
        }
        monotonic(&self.analyst, usage)?;
        let mut next = self.clone();
        next.analyst = usage.clone();
        next.terminal = true;
        next.validate(limits)?;
        Ok(next)
    }

    /// Settle the single dispatched audit, including a terminal timeout sample.
    /// This records consumed resources but cannot authorize publication or retry.
    pub fn stop_auditor(
        &self,
        usage: &InvestigationUsage,
        limits: &InvestigationLimits,
    ) -> Result<Self, InvestigationError> {
        self.validate(limits)?;
        if self.terminal
            || !self.audit_started
            || usage.model_invocations != 1
            || usage.generated_token_reservations != limits.generated_tokens_per_invocation
        {
            return Err(InvestigationError::InvalidInput);
        }
        monotonic(&self.auditor, usage)?;
        let mut next = self.clone();
        next.auditor = usage.clone();
        next.terminal = true;
        next.validate(limits)?;
        Ok(next)
    }

    /// Stop after owner loss without inventing another elapsed-time or usage
    /// sample. The already reserved invocation is retained and cannot restart.
    pub fn interrupt_audit(
        &self,
        limits: &InvestigationLimits,
    ) -> Result<Self, InvestigationError> {
        self.stop_auditor(&self.auditor, limits)
    }

    /// Validate after decoding against the host's original limits, never limits
    /// supplied by a child. Active time is aggregate; wall deadline stays host-owned.
    pub fn validate(&self, limits: &InvestigationLimits) -> Result<(), InvestigationError> {
        if self.schema_version != 1
            || self.limits_hash != limits_hash(limits)?
            || (!self.audit_started && self.auditor != InvestigationUsage::default())
            || (self.audit_started
                && (self.auditor.model_invocations != 1
                    || self.auditor.generated_token_reservations
                        != limits.generated_tokens_per_invocation))
        {
            return Err(InvestigationError::InvalidInput);
        }
        let mut usage = self.usage()?;
        if !self.audit_started && !self.terminal {
            usage.model_invocations = usage
                .model_invocations
                .checked_add(1)
                .ok_or(InvestigationError::LimitExceeded)?;
            usage.generated_token_reservations = usage
                .generated_token_reservations
                .checked_add(limits.generated_tokens_per_invocation)
                .ok_or(InvestigationError::LimitExceeded)?;
        }
        if usage.model_invocations > limits.model_invocations
            || usage.tool_actions > limits.tool_actions
            || usage.selected_facts > limits.selected_facts
            || usage.evidence_requests > limits.evidence_requests
            || usage.evidence_items > limits.evidence_items
            || usage.evidence_bytes > limits.evidence_bytes
            || usage.captured_validation_bytes > limits.captured_validation_bytes
            || usage.generated_token_reservations > limits.generated_token_reservations
            || (!self.terminal
                && usage.active_milliseconds
                    > limits
                        .active_seconds
                        .checked_mul(1000)
                        .ok_or(InvestigationError::LimitExceeded)?)
        {
            return Err(InvestigationError::LimitExceeded);
        }
        Ok(())
    }
}

fn limits_hash(limits: &InvestigationLimits) -> Result<String, InvestigationError> {
    Ok(core_prov::content_hash(&bounded_bytes(
        limits,
        MAX_RECORD_BYTES,
    )?))
}

fn monotonic(
    before: &InvestigationUsage,
    after: &InvestigationUsage,
) -> Result<(), InvestigationError> {
    macro_rules! check { ($($field:ident),*) => { $(if after.$field < before.$field {
        return Err(InvestigationError::InvalidInput);
    })* }; }
    check!(
        model_invocations,
        tool_actions,
        selected_facts,
        evidence_requests,
        evidence_items,
        evidence_bytes,
        captured_validation_bytes,
        generated_token_reservations,
        active_milliseconds
    );
    // Missing usage stays unknown; a later provider report cannot revise a
    // previously known cumulative total downwards or erase it.
    for (old, new) in [
        (before.reported_input_tokens, after.reported_input_tokens),
        (before.reported_output_tokens, after.reported_output_tokens),
        (
            before.reported_cache_read_tokens,
            after.reported_cache_read_tokens,
        ),
    ] {
        if old.is_some_and(|old| new.is_none_or(|new| new < old)) {
            return Err(InvestigationError::InvalidInput);
        }
    }
    Ok(())
}

fn sum(
    a: &InvestigationUsage,
    b: &InvestigationUsage,
) -> Result<InvestigationUsage, InvestigationError> {
    let mut result = InvestigationUsage::default();
    macro_rules! add { ($($field:ident),*) => { $(result.$field = a.$field.checked_add(b.$field)
        .ok_or(InvestigationError::LimitExceeded)?;)* }; }
    add!(
        model_invocations,
        tool_actions,
        selected_facts,
        evidence_requests,
        evidence_items,
        evidence_bytes,
        captured_validation_bytes,
        generated_token_reservations,
        active_milliseconds
    );
    let reported = |x: Option<u64>, y: Option<u64>| -> Result<Option<u64>, InvestigationError> {
        // An unused stage contributes nothing; preserve the existing stage's
        // None versus Some(0) representation rather than inventing a report.
        if b.model_invocations == 0 {
            return Ok(x);
        }
        if a.model_invocations == 0 {
            return Ok(y);
        }
        match (x, y) {
            (Some(x), Some(y)) => x
                .checked_add(y)
                .map(Some)
                .ok_or(InvestigationError::LimitExceeded),
            _ => Ok(None),
        }
    };
    result.reported_input_tokens = reported(a.reported_input_tokens, b.reported_input_tokens)?;
    result.reported_output_tokens = reported(a.reported_output_tokens, b.reported_output_tokens)?;
    result.reported_cache_read_tokens =
        reported(a.reported_cache_read_tokens, b.reported_cache_read_tokens)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_account_terminal_timeout_keeps_measurement_without_restart() {
        let limits = INVESTIGATION_V2_POLICY.limits();
        let account = SemanticAuditAccount::new(&limits).unwrap();
        assert_eq!(account.usage().unwrap(), InvestigationUsage::default());
        let usage = InvestigationUsage {
            model_invocations: 1,
            active_milliseconds: limits.active_seconds * 1000 + 1,
            ..InvestigationUsage::default()
        };
        assert!(account.analyst_usage(&usage, &limits).is_err());
        let stopped = account.stop_analyst(&usage, &limits).unwrap();
        stopped.validate(&limits).unwrap();
        assert_eq!(
            stopped.usage().unwrap().active_milliseconds,
            usage.active_milliseconds
        );
        assert!(stopped.begin_audit(&limits).is_err());
        assert!(stopped.analyst_usage(&usage, &limits).is_err());
    }

    // AC-0566: a timed-out audit remains charged after reopening, without retry.
    #[test]
    fn audit_account_terminal_auditor_keeps_charge_and_enforces_hard_limits() {
        let limits = INVESTIGATION_V2_POLICY.limits();
        let initial = SemanticAuditAccount::new(&limits).unwrap();
        let mut usage = InvestigationUsage {
            model_invocations: 1,
            generated_token_reservations: limits.generated_tokens_per_invocation,
            active_milliseconds: limits.active_seconds * 1000 + 1,
            ..InvestigationUsage::default()
        };
        assert!(initial.stop_auditor(&usage, &limits).is_err());
        let started = initial.begin_audit(&limits).unwrap();
        assert!(started.auditor_usage(&usage, &limits).is_err());
        let stopped = started.stop_auditor(&usage, &limits).unwrap();
        let bytes = serde_json::to_vec(&stopped).unwrap();
        let reopened: SemanticAuditAccount = serde_json::from_slice(&bytes).unwrap();
        reopened.validate(&limits).unwrap();
        assert_eq!(reopened.usage().unwrap(), usage);
        assert!(reopened.begin_audit(&limits).is_err());
        assert!(reopened.auditor_usage(&usage, &limits).is_err());
        assert!(reopened.stop_auditor(&usage, &limits).is_err());
        assert!(reopened.stop_analyst(&usage, &limits).is_err());
        usage.evidence_bytes = limits.evidence_bytes + 1;
        assert!(started.stop_auditor(&usage, &limits).is_err());
        usage.evidence_bytes = 0;
        usage.model_invocations = 0;
        assert!(started.stop_auditor(&usage, &limits).is_err());
        assert_eq!(started.usage().unwrap().model_invocations, 1);
    }

    // AC-0566: accounting never creates a second task's resource allowance.
    #[test]
    fn audit_account_retains_one_call_and_charges_it_once() {
        let limits = INVESTIGATION_V2_POLICY.limits();
        let account = SemanticAuditAccount::new(&limits).unwrap();
        assert_eq!(account.usage().unwrap(), InvestigationUsage::default());
        let usage = InvestigationUsage {
            model_invocations: limits.model_invocations,
            ..InvestigationUsage::default()
        };
        assert!(account.analyst_usage(&usage, &limits).is_err());
        let usage = InvestigationUsage {
            model_invocations: limits.model_invocations - 1,
            ..InvestigationUsage::default()
        };
        let account = account.analyst_usage(&usage, &limits).unwrap();
        let started = account.begin_audit(&limits).unwrap();
        assert_eq!(
            started.usage().unwrap().model_invocations,
            limits.model_invocations
        );
        assert_eq!(
            started.usage().unwrap().generated_token_reservations,
            limits.generated_tokens_per_invocation
        );
        assert!(started.begin_audit(&limits).is_err());
        assert!(started.analyst_usage(&usage, &limits).is_err());
    }

    #[test]
    fn audit_account_reopen_preserves_time_and_rejects_reset_or_limit_change() {
        let limits = INVESTIGATION_V2_POLICY.limits();
        let analyst = InvestigationUsage {
            model_invocations: 1,
            active_milliseconds: limits.active_seconds * 1000 - 1,
            evidence_bytes: limits.evidence_bytes,
            ..InvestigationUsage::default()
        };
        let account = SemanticAuditAccount::new(&limits)
            .unwrap()
            .analyst_usage(&analyst, &limits)
            .unwrap()
            .begin_audit(&limits)
            .unwrap();
        let saved = serde_json::to_vec(&account).unwrap();
        let reopened: SemanticAuditAccount = serde_json::from_slice(&saved).unwrap();
        assert_eq!(account, reopened);
        reopened.validate(&limits).unwrap();
        let mut audit = InvestigationUsage {
            model_invocations: 1,
            generated_token_reservations: limits.generated_tokens_per_invocation,
            active_milliseconds: 2,
            ..InvestigationUsage::default()
        };
        assert!(reopened.auditor_usage(&audit, &limits).is_err());
        audit.active_milliseconds = 1;
        audit.evidence_bytes = 1;
        assert!(reopened.auditor_usage(&audit, &limits).is_err());
        audit.evidence_bytes = 0;
        let settled = reopened.auditor_usage(&audit, &limits).unwrap();
        assert_eq!(
            settled.usage().unwrap().active_milliseconds,
            limits.active_seconds * 1000
        );
        audit.active_milliseconds = 0;
        assert!(settled.auditor_usage(&audit, &limits).is_err());
        let mut changed = limits.clone();
        changed.active_seconds += 1;
        assert!(settled.validate(&changed).is_err());
        assert_eq!(serde_json::to_vec(&account).unwrap(), saved);
    }
}
