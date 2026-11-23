use super::*;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryCursor {
    version: u32,
    namespace: String,
    conversation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    search_hash: Option<String>,
    #[serde(default)]
    active_only: bool,
    high: i64,
    before: i64,
}

/// App-only response extension: never serialized into StoredTask or MCP reads.
#[derive(Serialize)]
pub struct InvestigationDetailRead {
    #[serde(flatten)]
    pub detail: InvestigationDetail,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reported_usage_exact: Option<ReportedTaskUsage>,
    /// ADR-0085: the tool-calling runtime the run executed on, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<super::RunRuntime>,
    /// #2475: the node and edge counts of the prepared scope, if it held one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_size: Option<super::ScopeSize>,
}

#[derive(Serialize)]
pub struct ReportedTaskUsage {
    pub input_tokens: Option<String>,
    pub output_tokens: Option<String>,
    pub cache_read_tokens: Option<String>,
}

impl InvestigationDetailRead {
    /// Takes the already validated detail without cloning its nested owners.
    pub fn from_detail(detail: InvestigationDetail) -> Result<Self, InvestigationStoreError> {
        let base_bytes =
            context_hub::logical::count(&detail).ok_or(InvestigationStoreError::Invalid)?;
        if base_bytes > MAX_RECORD_BYTES {
            return Err(InvestigationStoreError::Capacity);
        }
        // Three nullable u64 strings add at most 60 payload bytes plus their
        // fixed typed slots. Count serialization without a full JSON buffer.
        let usage = &detail.usage;
        let reported_usage_exact = Some(ReportedTaskUsage {
            input_tokens: usage.reported_input_tokens.map(|value| value.to_string()),
            output_tokens: usage.reported_output_tokens.map(|value| value.to_string()),
            cache_read_tokens: usage
                .reported_cache_read_tokens
                .map(|value| value.to_string()),
        });
        let mut read = Self {
            detail,
            reported_usage_exact,
            runtime: None,
            scope_size: None,
        };
        let bytes = context_hub::logical::count(&read).ok_or(InvestigationStoreError::Invalid)?;
        if bytes > MAX_RECORD_BYTES {
            read.reported_usage_exact = None;
        }
        Ok(read)
    }

    /// Adds the run's runtime identity when the read stays within its cap.
    pub fn with_runtime(mut self, runtime: Option<super::RunRuntime>) -> Self {
        self.runtime = runtime;
        if context_hub::logical::count(&self).is_none_or(|bytes| bytes > MAX_RECORD_BYTES) {
            self.runtime = None;
        }
        self
    }

    /// Adds the prepared scope's size when the read stays within its cap.
    pub fn with_scope_size(mut self, scope_size: Option<super::ScopeSize>) -> Self {
        self.scope_size = scope_size;
        if context_hub::logical::count(&self).is_none_or(|bytes| bytes > MAX_RECORD_BYTES) {
            self.scope_size = None;
        }
        self
    }
}

/// App-only activity attribution projected from the validated saved task.
#[derive(Debug, Clone, Serialize)]
pub struct SavedActivityProvider {
    pub mode: InvestigationProviderMode,
    pub provider_id: String,
    pub model: String,
}

/// Exact display values, not a price or complete lifetime accounting claim.
#[derive(Debug, Clone, Serialize)]
pub struct ReportedActivityUsage {
    pub input_tokens: Option<String>,
    pub output_tokens: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpecialistActivityItem {
    pub summary: InvestigationSummary,
    pub saved_provider: SavedActivityProvider,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reported_usage: Option<ReportedActivityUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reported_cache_read_tokens: Option<Option<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpecialistActivityPage {
    pub items: Vec<SpecialistActivityItem>,
    pub next_cursor: Option<String>,
}

#[derive(Serialize)]
struct SavedActivityProviderProjection<'a> {
    mode: InvestigationProviderMode,
    provider_id: &'a str,
    model: &'a str,
}

#[derive(Serialize)]
struct SpecialistActivityProjection<'a> {
    summary: &'a InvestigationSummary,
    saved_provider: SavedActivityProviderProjection<'a>,
}

pub(super) fn encode_specialist_activity_item(
    summary: &InvestigationSummary,
    provider: &InvestigationProvider,
) -> Result<Vec<u8>, InvestigationStoreError> {
    if provider.mode != summary.provider_mode {
        return Err(InvestigationStoreError::Invalid);
    }
    encode(
        &SpecialistActivityProjection {
            summary,
            saved_provider: SavedActivityProviderProjection {
                mode: provider.mode,
                provider_id: &provider.provider_id,
                model: &provider.model,
            },
        },
        4096,
    )
}

/// Reject a new task unless its activity identity and a maximum valid mutable
/// summary still fit the existing 4 KiB per-item projection bound.
pub(super) fn specialist_activity_admission_bytes(
    task: &StoredTask,
) -> Result<Vec<u8>, InvestigationStoreError> {
    let mut summary = task.detail.summary.clone();
    // Reserve the largest individually valid wire values. The combined sizing
    // envelope is intentionally conservative; it need not be a reachable state.
    summary.status = InvestigationStatus::AwaitingConsent;
    summary.revision = i64::MAX as u64;
    // JSON `false` occupies five bytes; `true` occupies four.
    summary.cancel_requested = false;
    summary.invocation_pending = false;
    summary.actions.can_cancel = false;
    summary.actions.can_follow_up = true;
    summary.graph_snapshot_id = Some(format!("context-v1:{}", "f".repeat(64)));
    summary.last_event_sequence = journal_event_limit(task)?;
    summary.has_result = false;
    if task.detail.inspection_requirements.is_some() {
        summary.inspection_status = Some(InvestigationInspectionStatus::NotAssessed);
        let max_requirements = super::policy_for_detail(&task.detail)?.max_completion_obligations();
        summary.inspection_requirement_count = Some(max_requirements);
    }
    if task.completion_intent.is_some() {
        summary.completion_outcome = Some(InvestigationCompletionOutcome::InsufficientEvidence);
        summary.obligation_status = Some(CompletionStatus::NotAssessed);
    }
    // Host-generated timestamps use a fixed 20-byte UTC form. Keep admission
    // tied to that emitted form rather than the looser legacy read validator.
    summary.updated_at = "9".repeat(20);
    encode_specialist_activity_item(&summary, &task.detail.provider)
}

pub(super) fn validate_specialist_activity_admission(
    task: &StoredTask,
) -> Result<(), InvestigationStoreError> {
    specialist_activity_admission_bytes(task).map(|_| ())
}

#[derive(Serialize)]
struct HistoryProjectionPage<T> {
    items: Vec<T>,
    next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    answer_match_ids: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    name_match_ids: Vec<String>,
}

impl JobStore {
    pub fn investigation_history(
        &self,
        conversation_id: Option<&str>,
        cursor: Option<&str>,
    ) -> Result<InvestigationPage, InvestigationStoreError> {
        self.investigation_history_filtered(conversation_id, None, false, cursor)
    }

    /// The saved specialist, provider mode and status of every in-flight
    /// investigation, read through the active selection for the agent roster
    /// (AC-1408). `None` when more than `max_pages` pages are in flight.
    pub fn active_investigation_work(
        &self,
        max_pages: usize,
    ) -> Result<Option<Vec<agents::roster::AgentWork>>, InvestigationStoreError> {
        let mut work = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..max_pages {
            let page = self.investigation_history_selection(None, true, cursor.as_deref())?;
            work.extend(
                page.items
                    .iter()
                    .map(|item| (item.specialist_id, item.provider_mode, item.status)),
            );
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(Some(work)),
            }
        }
        Ok(None)
    }

    pub fn search_investigation_history(
        &self,
        query: &str,
        cursor: Option<&str>,
    ) -> Result<InvestigationPage, InvestigationStoreError> {
        self.investigation_history_selection(Some(query), false, cursor)
    }

    pub fn investigation_history_selection(
        &self,
        search: Option<&str>,
        active_only: bool,
        cursor: Option<&str>,
    ) -> Result<InvestigationPage, InvestigationStoreError> {
        let query = search
            .map(|query| {
                if query.len() > agents::investigation::MAX_QUESTION_BYTES {
                    return Err(InvestigationStoreError::Invalid);
                }
                let query = query.trim_matches(|character: char| {
                    matches!(
                        character,
                        '\t' | '\n' | '\u{000b}' | '\u{000c}' | '\r' | ' '
                    )
                });
                let query = if query.is_empty() {
                    None
                } else {
                    let mut normalized = query.as_bytes().to_vec();
                    normalized.make_ascii_lowercase();
                    Some(normalized)
                };
                Ok(query)
            })
            .transpose()?
            .flatten();
        self.investigation_history_filtered(None, query.as_deref(), active_only, cursor)
    }

    pub fn specialist_task_activity(
        &self,
        cursor: Option<&str>,
    ) -> Result<SpecialistActivityPage, InvestigationStoreError> {
        let page = self.investigation_history_projected(None, None, true, cursor, |task| {
            // Preserve base validation/admission and legacy serialization before
            // attempting optional telemetry; only its capacity may be omitted.
            encode_specialist_activity_item(&task.detail.summary, &task.detail.provider)?;
            let reported_usage = ReportedActivityUsage {
                input_tokens: task
                    .detail
                    .usage
                    .reported_input_tokens
                    .map(|value| value.to_string()),
                output_tokens: task
                    .detail
                    .usage
                    .reported_output_tokens
                    .map(|value| value.to_string()),
            };
            let cache_read = task
                .detail
                .usage
                .reported_cache_read_tokens
                .map(|value| value.to_string());
            let provider = task.detail.provider;
            let mut item = SpecialistActivityItem {
                summary: task.detail.summary,
                saved_provider: SavedActivityProvider {
                    mode: provider.mode,
                    provider_id: provider.provider_id,
                    model: provider.model,
                },
                reported_usage: Some(reported_usage),
                reported_cache_read_tokens: Some(cache_read),
            };
            // Preserve existing input/output metadata before omitting it: the
            // additive cache projection is the first optional capacity fallback.
            match encode(&item, 4096) {
                Ok(_) => {}
                Err(InvestigationStoreError::Capacity) => {
                    item.reported_cache_read_tokens = None;
                    match encode(&item, 4096) {
                        Ok(_) => {}
                        Err(InvestigationStoreError::Capacity) => item.reported_usage = None,
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            }
            Ok(item)
        })?;
        Ok(SpecialistActivityPage {
            items: page.items,
            next_cursor: page.next_cursor,
        })
    }

    fn investigation_history_filtered(
        &self,
        conversation_id: Option<&str>,
        query: Option<&[u8]>,
        active_only: bool,
        cursor: Option<&str>,
    ) -> Result<InvestigationPage, InvestigationStoreError> {
        let page = self.investigation_history_projected(
            conversation_id,
            query,
            active_only,
            cursor,
            |task| Ok(task.detail.summary),
        )?;
        Ok(InvestigationPage {
            items: page.items,
            next_cursor: page.next_cursor,
            answer_match_ids: page.answer_match_ids,
            name_match_ids: page.name_match_ids,
        })
    }

    fn investigation_history_projected<T: Serialize>(
        &self,
        conversation_id: Option<&str>,
        query: Option<&[u8]>,
        active_only: bool,
        cursor: Option<&str>,
        project: impl Fn(StoredTask) -> Result<T, InvestigationStoreError>,
    ) -> Result<HistoryProjectionPage<T>, InvestigationStoreError> {
        if conversation_id.is_some_and(|id| !id_valid(id)) {
            return Err(InvestigationStoreError::Invalid);
        }
        let search_hash = query.map(core_prov::content_hash);
        let cursor_version = if active_only {
            3
        } else if search_hash.is_some() {
            2
        } else {
            1
        };
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let selection = match cursor {
            Some(cursor) => {
                if cursor.len() > 512 {
                    return Err(InvestigationStoreError::Invalid);
                }
                let cursor: HistoryCursor = decode_record(cursor)?;
                if cursor.version != cursor_version
                    || cursor.namespace != self.namespace.value
                    || cursor.conversation.as_deref() != conversation_id
                    || cursor.search_hash != search_hash
                    || cursor.active_only != active_only
                    || cursor.high < 0
                    || cursor.before <= 0
                    || cursor.before > cursor.high
                {
                    return Err(InvestigationStoreError::Invalid);
                }
                cursor
            }
            None => {
                let high: i64 = tx.query_row(
                    "SELECT coalesce(max(ordinal),0) FROM investigation_tasks",
                    [],
                    |r| r.get(0),
                )?;
                HistoryCursor {
                    version: cursor_version,
                    namespace: self.namespace.value.clone(),
                    conversation: conversation_id.map(str::to_owned),
                    search_hash: search_hash.clone(),
                    active_only,
                    high,
                    before: high
                        .checked_add(1)
                        .ok_or(InvestigationStoreError::Capacity)?,
                }
            }
        };
        let candidate_limit = if query.is_some() || active_only {
            MAX_TASKS + 1
        } else {
            51
        };
        let rows=tx.prepare("SELECT ordinal,CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))<=128 THEN id END FROM investigation_tasks WHERE ordinal<=?1 AND ordinal<?2 AND (?3 IS NULL OR conversation_id=?3) ORDER BY ordinal DESC LIMIT ?4")?
            .query_map(params![selection.high,selection.before,conversation_id,i64::try_from(candidate_limit).map_err(|_| InvestigationStoreError::Capacity)?],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,Option<String>>(1)?)))?
            .collect::<Result<Vec<_>,_>>()?;
        if (query.is_some() || active_only) && rows.len() > MAX_TASKS {
            return Err(InvestigationStoreError::Capacity);
        }
        let mut items = Vec::new();
        let mut answer_match_ids = Vec::new();
        let mut name_match_ids = Vec::new();
        let mut remaining = 256 * 1024 - 4096;
        let mut before = selection.before;
        let mut more = false;
        for (ordinal, id) in rows {
            let id = id.ok_or(InvestigationStoreError::Invalid)?;
            if ordinal <= 0 {
                return Err(InvestigationStoreError::Invalid);
            }
            let task = load_execution_task(&tx, &id)?;
            let summary = &task.detail.summary;
            if active_only && !investigation_status_is_active(summary.status) {
                if items.len() < 50 {
                    before = ordinal;
                }
                continue;
            }
            // Additional answer matching is only needed for actual search metadata misses.
            // Existing task validation (including its saved-result reads) is unchanged.
            // Each result and its validation owners are released before the next task.
            let (answer_match, name_match) = if let Some(query) = query {
                if history_summary_matches(summary, query)? {
                    (false, false)
                } else if ascii_case_insensitive_contains(
                    task.detail.specialist.name.as_bytes(),
                    query,
                ) {
                    (false, true)
                } else {
                    let matches = load_result(&tx, &task)?.is_some_and(|result| {
                        result.narrative.as_ref().is_some_and(|message| {
                            ascii_case_insensitive_contains(message.text.as_bytes(), query)
                        }) || result.findings.iter().any(|finding| {
                            ascii_case_insensitive_contains(finding.title.as_bytes(), query)
                                || ascii_case_insensitive_contains(
                                    finding.statement.as_bytes(),
                                    query,
                                )
                        })
                    });
                    if !matches {
                        if items.len() < 50 {
                            before = ordinal;
                        }
                        continue;
                    }
                    (true, false)
                }
            } else {
                (false, false)
            };
            if query.is_some() && items.len() == 50 {
                more = true;
                break;
            }
            let item = project(task)?;
            let size = encode(&item, 4096)?.len()
                + 1
                + if answer_match || name_match {
                    encode(&id, 4096)?.len() + 1
                } else {
                    0
                };
            if items.len() == 50 || size > remaining {
                more = true;
                break;
            }
            remaining -= size;
            before = ordinal;
            items.push(item);
            if answer_match {
                answer_match_ids.push(id);
            } else if name_match {
                name_match_ids.push(id);
            }
        }
        let next_cursor = if more {
            Some(
                String::from_utf8(encode(
                    &HistoryCursor {
                        before,
                        ..selection
                    },
                    512,
                )?)
                .map_err(|_| InvestigationStoreError::Invalid)?,
            )
        } else {
            None
        };
        let page = HistoryProjectionPage {
            items,
            next_cursor,
            answer_match_ids,
            name_match_ids,
        };
        encode(&page, 256 * 1024)?;
        tx.commit()?;
        Ok(page)
    }

    pub fn investigation_events(
        &self,
        id: &str,
        after_sequence: u64,
    ) -> Result<InvestigationEventPage, InvestigationStoreError> {
        self.investigation_events_access(id, after_sequence, None)
    }

    pub fn investigation_events_access(
        &self,
        id: &str,
        after_sequence: u64,
        session: Option<&McpTaskSession>,
    ) -> Result<InvestigationEventPage, InvestigationStoreError> {
        with_task_access(session, |client| {
            let tx = self.conn.unchecked_transaction()?;
            validate(&tx, &self.namespace)?;
            require_request_owner(&tx, id, client)?;
            let task = load_execution_task(&tx, id)?;
            let page = event_page(&tx, &task, id, after_sequence)?;
            tx.commit()?;
            Ok(page)
        })
    }

    /// Detail, result, ledger and the whole event journal from one validated
    /// snapshot: the same checks as the separate reads, with one task load
    /// instead of one per read and per event page.
    #[cfg(any(test, feature = "test-support"))]
    pub fn investigation_snapshot(
        &self,
        id: &str,
    ) -> Result<InvestigationSnapshot, InvestigationStoreError> {
        self.snapshot(id, false).map(|(snapshot, _)| snapshot)
    }

    /// `investigation_snapshot` plus the refusal diagnostics, from the same
    /// transaction and task load.
    #[cfg(any(test, feature = "test-support"))]
    pub fn investigation_receipt_snapshot(
        &self,
        id: &str,
    ) -> Result<ReceiptSnapshot, InvestigationStoreError> {
        self.snapshot(id, true)
    }

    #[cfg(any(test, feature = "test-support"))]
    fn snapshot(
        &self,
        id: &str,
        diagnostics: bool,
    ) -> Result<ReceiptSnapshot, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        require_request_owner(&tx, id, None)?;
        let task = load_execution_task(&tx, id)?;
        let result = load_result(&tx, &task)?;
        let ledger = load_ledger(&tx, &task)?;
        let mut events = Vec::new();
        let mut after = 0;
        loop {
            let page = event_page(&tx, &task, id, after)?;
            events.extend(page.items);
            if !page.has_more {
                break;
            }
            if page.next_sequence <= after {
                return Err(InvestigationStoreError::Invalid);
            }
            after = page.next_sequence;
        }
        let sidecars = if diagnostics {
            super::refusal_diagnostics::project_loaded(&tx, &task, id, result.as_ref())?
        } else {
            Vec::new()
        };
        tx.commit()?;
        Ok(((task.detail, result, ledger, events), sidecars))
    }

    pub fn investigation_receipt_references(
        &self,
        source_id: &str,
    ) -> Result<Vec<InvestigationReceiptUse>, InvestigationStoreError> {
        if !id_valid(source_id) {
            return Err(InvestigationStoreError::Invalid);
        }
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        // Reconcile every immutable admitted ledger before applying the source
        // filter. Checking only matching index rows cannot detect a deleted or
        // mislabelled reference, even when the canonical triggers are restored.
        let (tasks, inputs, refs): (i64, i64, i64) = tx.query_row(
            "SELECT (SELECT count(*) FROM investigation_tasks), (SELECT count(*) FROM investigation_inputs), (SELECT count(*) FROM investigation_refs)",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if usize::try_from(tasks)
            .ok()
            .is_none_or(|count| count > MAX_TASKS)
        {
            return Err(InvestigationStoreError::Capacity);
        }
        let ids = tx.prepare("SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))<=128 THEN id END FROM investigation_tasks ORDER BY id")?
            .query_map([], |row| row.get::<_, Option<String>>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        // Only registry-resolved immutable policies contribute admission caps.
        // Inspect at most MAX_TASKS bounded records before owning input/ref rows;
        // a new registered profile cannot inherit a hard-coded legacy ceiling.
        let mut loaded = Vec::with_capacity(ids.len());
        let mut input_capacity = 0usize;
        let mut reference_capacity = 0usize;
        for id in ids {
            let id = id.ok_or(InvestigationStoreError::Invalid)?;
            let task = load_execution_task(&tx, &id)?;
            let policy = super::policy_for_detail(&task.detail)?;
            let revisions = usize::try_from(policy.limits().tool_actions)
                .ok()
                .and_then(|count| count.checked_add(1))
                .ok_or(InvestigationStoreError::Capacity)?;
            input_capacity = input_capacity
                .checked_add(revisions)
                .ok_or(InvestigationStoreError::Capacity)?;
            // Ledger receipts are monotonic and bounded by selected_facts. Pins
            // may retain additional receipts before or outside that selection.
            reference_capacity = reference_capacity
                .checked_add(policy.limits().selected_facts)
                .and_then(|count| count.checked_add(policy.max_completion_obligations()))
                .ok_or(InvestigationStoreError::Capacity)?;
            loaded.push(task);
        }
        if usize::try_from(inputs)
            .ok()
            .is_none_or(|count| count > input_capacity)
            || usize::try_from(refs)
                .ok()
                .is_none_or(|count| count > reference_capacity)
        {
            return Err(InvestigationStoreError::Capacity);
        }
        type ReferenceKey = (String, String, String, String);
        let mut expected = std::collections::BTreeMap::<ReferenceKey, i64>::new();
        let mut checked_inputs = 0i64;
        for task in loaded {
            let id = task.detail.summary.investigation_id.clone();
            let policy = super::policy_for_detail(&task.detail)?;
            if let Some(targets) = &task.detail.inspection_requirements {
                for target in targets {
                    expected
                        .entry((
                            id.clone(),
                            target.registered_source_id.clone(),
                            target.source.repo.clone(),
                            target.receipt_id.clone(),
                        ))
                        .or_insert(0);
                }
            }
            if let Some(intent) = &task.completion_intent {
                for target in intent.obligations.sources_with_policy(&policy)? {
                    // The actual factory inserts pinned source receipts at
                    // revision zero, even while the task has no initial ledger.
                    expected
                        .entry((
                            id.clone(),
                            target.registered_source_id,
                            target.source.repo,
                            target.receipt_id,
                        ))
                        .or_insert(0);
                }
            }
            // Verify the current pointer/hash as well as the independently
            // retained revisions. Historical references are never subtracted.
            let current = load_ledger(&tx, &task)?;
            let max_revisions = u64::from(policy.limits().tool_actions)
                .checked_add(1)
                .ok_or(InvestigationStoreError::Invalid)?;
            let query_limit = max_revisions
                .checked_add(1)
                .ok_or(InvestigationStoreError::Invalid)?;
            let revisions = tx.prepare("SELECT revision FROM investigation_inputs WHERE task_id=?1 ORDER BY revision LIMIT ?2")?
                .query_map(params![&id, signed(query_limit)?], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            if revisions.len() as u64 > max_revisions
                || revisions.last().copied() != task.ledger_revision.map(signed).transpose()?
            {
                return Err(InvestigationStoreError::Invalid);
            }
            for revision in revisions {
                checked_inputs += 1;
                let ledger: InvestigationInputLedger =
                    read_body(&tx, "investigation_inputs", "revision", &id, &revision)?
                        .ok_or(InvestigationStoreError::Invalid)?;
                ledger.validate_for_specialist(task.detail.summary.specialist_id, &policy)?;
                encode(&ledger, MAX_MANIFEST_BYTES)?;
                if signed(ledger.revision)? != revision
                    || Some(&ledger.graph_snapshot_id)
                        != task.detail.summary.graph_snapshot_id.as_ref()
                    || Some(&ledger.scope_snapshot_id) != task.detail.scope_snapshot_id.as_ref()
                    || current.as_ref().is_none_or(|latest| {
                        !ledger
                            .receipt_references
                            .iter()
                            .all(|reference| latest.receipt_references.contains(reference))
                    })
                {
                    return Err(InvestigationStoreError::Invalid);
                }
                for reference in ledger.receipt_references {
                    expected
                        .entry((
                            id.clone(),
                            reference.source_id,
                            reference.repo_key,
                            reference.receipt_id,
                        ))
                        .or_insert(revision);
                    if expected.len() > reference_capacity {
                        return Err(InvestigationStoreError::Capacity);
                    }
                }
            }
        }
        if checked_inputs != inputs || i64::try_from(expected.len()).ok() != Some(refs) {
            return Err(InvestigationStoreError::Invalid);
        }
        let rows = tx.prepare("SELECT CASE WHEN typeof(task_id)='text' AND length(CAST(task_id AS BLOB))<=128 THEN task_id END,input_revision,CASE WHEN typeof(source_id)='text' AND length(CAST(source_id AS BLOB))<=256 THEN source_id END,CASE WHEN typeof(repo_key)='text' AND length(CAST(repo_key AS BLOB))<=256 THEN repo_key END,CASE WHEN typeof(receipt_id)='text' AND length(CAST(receipt_id AS BLOB))<=256 THEN receipt_id END FROM investigation_refs ORDER BY task_id,source_id,repo_key,receipt_id")?
            .query_map([], |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, Option<String>>(4)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut references = Vec::new();
        for (task_id, input_revision, indexed_source, repo_key, receipt_id) in rows {
            let (Some(task_id), Some(indexed_source), Some(repo_key), Some(receipt_id)) =
                (task_id, indexed_source, repo_key, receipt_id)
            else {
                return Err(InvestigationStoreError::Invalid);
            };
            let key = (
                task_id.clone(),
                indexed_source.clone(),
                repo_key.clone(),
                receipt_id.clone(),
            );
            if expected.remove(&key) != Some(input_revision) {
                return Err(InvestigationStoreError::Invalid);
            }
            if indexed_source == source_id {
                references.push(InvestigationReceiptUse {
                    investigation_id: task_id,
                    source_id: indexed_source,
                    repo_key,
                    receipt_id,
                });
            }
        }
        if !expected.is_empty() {
            return Err(InvestigationStoreError::Invalid);
        }
        tx.commit()?;
        Ok(references)
    }
}

pub(super) fn investigation_status_is_active(
    status: agents::investigation::InvestigationStatus,
) -> bool {
    matches!(
        status,
        agents::investigation::InvestigationStatus::Queued
            | agents::investigation::InvestigationStatus::Preparing
            | agents::investigation::InvestigationStatus::Running
            | agents::investigation::InvestigationStatus::AwaitingConsent
    )
}

fn history_summary_matches(
    summary: &agents::investigation::InvestigationSummary,
    query: &[u8],
) -> Result<bool, InvestigationStoreError> {
    let specialist = serde_json::to_value(summary.specialist_id)
        .map_err(|_| InvestigationStoreError::Invalid)?;
    let specialist = specialist
        .as_str()
        .ok_or(InvestigationStoreError::Invalid)?;
    Ok(
        ascii_case_insensitive_contains(summary.question.as_bytes(), query)
            || ascii_case_insensitive_contains(specialist.as_bytes(), query),
    )
}

fn ascii_case_insensitive_contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack.windows(needle.len()).any(|window| {
            window
                .iter()
                .zip(needle)
                .all(|(left, right)| left.eq_ignore_ascii_case(right))
        })
}

#[cfg(any(test, feature = "test-support"))]
#[path = "reads_tests.rs"]
mod tests;

#[cfg(any(test, feature = "test-support"))]
pub type InvestigationSnapshot = (
    InvestigationDetail,
    Option<InvestigationResult>,
    Option<InvestigationInputLedger>,
    Vec<InvestigationEvent>,
);

/// An `InvestigationSnapshot` with its refusal diagnostics.
#[cfg(any(test, feature = "test-support"))]
pub type ReceiptSnapshot = (
    InvestigationSnapshot,
    Vec<agents::investigation::diagnostic_sidecar::Sidecar>,
);

/// One bounded, validated page of `task`'s event journal after `after_sequence`.
fn event_page(
    tx: &Connection,
    task: &StoredTask,
    id: &str,
    after_sequence: u64,
) -> Result<InvestigationEventPage, InvestigationStoreError> {
    if after_sequence > journal_event_limit(task)? {
        return Err(InvestigationStoreError::Invalid);
    }
    let sequences=tx.prepare("SELECT sequence FROM investigation_events WHERE task_id=?1 AND sequence>?2 ORDER BY sequence LIMIT 51")?
    .query_map(params![id,signed(after_sequence)?],|r|r.get::<_,i64>(0))?.collect::<Result<Vec<_>,_>>()?;
    let mut items = Vec::new();
    let mut remaining = 256 * 1024 - 4096;
    let mut next_sequence = after_sequence;
    let mut has_more = false;
    for sequence in sequences {
        let event: InvestigationEvent =
            read_body(tx, "investigation_events", "sequence", id, &sequence)?
                .ok_or(InvestigationStoreError::Invalid)?;
        super::query_refusal::validate_event(tx, task, &event)?;
        if event.investigation_id != id
            || signed(event.sequence)? != sequence
            || event.sequence != next_sequence + 1
            || event.sequence > task.detail.summary.last_event_sequence
            || event.revision > task.detail.summary.revision
            || event.summary.len() > 1024
            || redacted_history_text(&event.summary, 1024)? != event.summary
            || event.step_id.as_ref().is_some_and(|v| !id_valid(v))
            || event.created_at.len() > 32
        {
            return Err(InvestigationStoreError::Invalid);
        }
        let size = encode(&event, 16 * 1024)?.len() + 1;
        if items.len() == 50 || size > remaining {
            has_more = true;
            break;
        }
        remaining -= size;
        next_sequence = event.sequence;
        items.push(event);
    }
    if !has_more && next_sequence < task.detail.summary.last_event_sequence {
        return Err(InvestigationStoreError::Invalid);
    }
    let page = InvestigationEventPage {
        investigation_id: id.into(),
        items,
        next_sequence,
        has_more,
    };
    encode(&page, 256 * 1024)?;
    Ok(page)
}
