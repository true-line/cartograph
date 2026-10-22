use super::*;
use serde::ser::{SerializeSeq, Serializer};

#[derive(Serialize)]
struct Reason<'a> {
    kind: context_hub::discovery::MatchKind,
    field: &'a str,
    value: &'a str,
}
#[derive(Serialize)]
struct Source<'a> {
    repo: Option<&'a str>,
    path: Option<&'a str>,
    commit_sha: Option<&'a str>,
}
#[derive(Serialize)]
struct Candidate<'a> {
    reference: &'a context_hub::FactReference,
    label: &'a str,
    source: Source<'a>,
    confidence_tier: core_prov::ConfidenceTier,
    provenance_problem: Option<context_hub::ProvenanceProblem>,
    tier: Option<core_prov::Tier>,
    extractor_id: Option<&'a str>,
    content_hash: Option<&'a str>,
    method: Option<&'a core_prov::ProducingMethod>,
    match_reason: Reason<'a>,
    outgoing_edges: Option<usize>,
    incoming_edges: Option<usize>,
}
impl Candidate<'_> {
    fn to_owned(&self) -> ScopedCandidate {
        ScopedCandidate {
            reference: self.reference.clone(),
            label: self.label.into(),
            source: context_hub::discovery::CandidateSource {
                repo: self.source.repo.map(str::to_owned),
                path: self.source.path.map(str::to_owned),
                commit_sha: self.source.commit_sha.map(str::to_owned),
            },
            confidence_tier: self.confidence_tier,
            provenance_problem: self.provenance_problem,
            tier: self.tier,
            extractor_id: self.extractor_id.map(str::to_owned),
            content_hash: self.content_hash.map(str::to_owned),
            method: self.method.cloned(),
            match_reason: MatchReason {
                kind: self.match_reason.kind,
                field: self.match_reason.field.into(),
                value: self.match_reason.value.into(),
            },
            outgoing_edges: None,
            incoming_edges: None,
        }
    }
}

struct Candidates<'a> {
    current: &'a [ScopedCandidate],
    next: Option<&'a Candidate<'a>>,
}
impl Serialize for Candidates<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq =
            s.serialize_seq(Some(self.current.len() + usize::from(self.next.is_some())))?;
        for candidate in self.current {
            seq.serialize_element(candidate)?;
        }
        if let Some(candidate) = self.next {
            seq.serialize_element(candidate)?;
        }
        seq.end()
    }
}
#[derive(Serialize)]
struct Cursor<'a> {
    query_basis_id: &'a str,
    after_id: &'a str,
}
#[derive(Serialize)]
struct Page<'a> {
    session_basis_id: &'a str,
    query_basis_id: &'a str,
    revision: &'a StoreRevision,
    coverage: &'a CoverageQualification,
    scanned_rows: usize,
    loaded_rows: usize,
    remaining: Remaining,
    complete_system: bool,
    remaining_rows: Option<usize>,
    candidates: Candidates<'a>,
    next_cursor: Option<Cursor<'a>>,
}
impl ScopedReadSession {
    pub(crate) fn discover(
        &self,
        ctx: &mut ScopedReadContext<'_>,
        request: ScopedDiscoveryRequest,
    ) -> Result<ScopedDiscoveryPage, ReadError> {
        if request.max_scan_rows == 0
            || request.max_scan_rows > self.graph_limits.max_rows
            || request.max_candidates == 0
            || request.max_candidates > MAX_CANDIDATES
            || request.max_bytes == 0
            || request.max_bytes > context_hub::MAX_RESPONSE_BYTES
        {
            return Err(ReadError::Limit);
        }
        self.check_current(ctx)?;
        let matcher = DiscoveryMatcher::new(&request.query).map_err(context_error)?;
        // Labels are transport-admitted before this set is materialized; the
        // matcher counts the complete normalized identity before hashing it.
        let labels: BTreeSet<_> = request.labels.into_iter().collect();
        let query = ScopedReadBasis::new(
            self.revision.clone(),
            matcher.query_digest(&labels).map_err(context_error)?,
            self.qualifications.clone(),
        )
        .map_err(|_| ReadError::Storage)?;
        if let Some(cursor) = &request.cursor {
            query
                .require_current(&cursor.query_basis_id)
                .map_err(|_| ReadError::StaleBasis)?;
        }
        let mut limits = self.graph_limits;
        limits.max_rows = request.max_scan_rows;
        let window = graph(
            ctx.graph
                .lock()
                .map_err(|_| ReadError::Storage)?
                .read_node_window(&NodeWindowRequest {
                    expected_revision: Some(self.revision.clone()),
                    after_id: request.cursor.as_ref().map(|c| c.after_id.clone()),
                    limits,
                }),
        )?;
        let total = window.rows.len();
        let mut candidates = Vec::new();
        let mut processed = 0;
        let mut after = request.cursor.as_ref().map(|c| c.after_id.clone());
        let mut remaining = window.remaining;
        for node in window.rows {
            let key = node.id.clone();
            let visible = !spec::project_recovered_facts(
                std::slice::from_ref(&node),
                &[],
                self.mode,
                &self.rejected,
            )
            .0
            .is_empty();
            let fact = ContextFact::from_node(node);
            let found = if visible && (labels.is_empty() || labels.contains(&fact.label)) {
                matcher.match_node(&key, &fact.properties)
            } else {
                None
            };
            let provenance = fact.provenance.as_ref();
            let evidence = provenance.and_then(|p| p.evidence.first());
            let staged = found.map(|m| Candidate {
                reference: &fact.reference,
                label: &fact.label,
                source: Source {
                    repo: evidence.map(|e| e.repo.as_str()),
                    path: evidence.map(|e| e.path.as_str()),
                    commit_sha: evidence.map(|e| e.commit_sha.as_str()),
                },
                confidence_tier: fact.confidence_tier,
                provenance_problem: fact.provenance_problem,
                tier: provenance.map(|p| p.tier),
                extractor_id: provenance.map(|p| p.extractor_id.as_str()),
                content_hash: provenance.map(|p| p.content_hash.as_str()),
                method: provenance.map(|p| &p.method),
                match_reason: Reason {
                    kind: m.kind,
                    field: m.field,
                    value: m.value,
                },
                outgoing_edges: None,
                incoming_edges: None,
            });
            let next_remaining = if processed + 1 < total {
                Remaining::More
            } else {
                window.remaining
            };
            let view = Page {
                session_basis_id: &self.basis_id,
                query_basis_id: query.id(),
                revision: &self.revision,
                coverage: &self.coverage,
                scanned_rows: processed + 1,
                loaded_rows: total,
                remaining: next_remaining,
                complete_system: false,
                remaining_rows: None,
                candidates: Candidates {
                    current: &candidates,
                    next: staged.as_ref(),
                },
                next_cursor: (next_remaining == Remaining::More).then_some(Cursor {
                    query_basis_id: query.id(),
                    after_id: &key,
                }),
            };
            if admission::fits(&view, request.max_bytes).is_err() {
                if processed == 0 {
                    return Err(ReadError::Limit);
                }
                remaining = Remaining::More;
                break;
            }
            // Candidate headers, source/provenance summaries and reason strings
            // are copied only after the complete response fits as a borrowed view.
            if let Some(staged) = staged {
                candidates.push(staged.to_owned());
            }
            processed += 1;
            after = Some(key);
            remaining = next_remaining;
            if candidates.len() == request.max_candidates {
                break;
            }
        }
        let cursor = if remaining == Remaining::More {
            Some(Cursor {
                query_basis_id: query.id(),
                after_id: after.as_deref().ok_or(ReadError::Limit)?,
            })
        } else {
            None
        };
        admission::fits(
            &Page {
                session_basis_id: &self.basis_id,
                query_basis_id: query.id(),
                revision: &self.revision,
                coverage: &self.coverage,
                scanned_rows: processed,
                loaded_rows: total,
                remaining,
                complete_system: false,
                remaining_rows: None,
                candidates: Candidates {
                    current: &candidates,
                    next: None,
                },
                next_cursor: cursor,
            },
            request.max_bytes,
        )?;
        let next_cursor = if remaining == Remaining::More {
            Some(ScopedCursor {
                query_basis_id: query.id().to_owned(),
                after_id: after.ok_or(ReadError::Limit)?,
            })
        } else {
            None
        };
        self.check_current(ctx)?;
        let response = ScopedDiscoveryPage {
            session_basis_id: self.basis_id.clone(),
            query_basis_id: query.id().into(),
            revision: self.revision.clone(),
            coverage: self.coverage.clone(),
            scanned_rows: processed,
            loaded_rows: total,
            remaining,
            complete_system: false,
            remaining_rows: None,
            candidates,
            next_cursor,
        };
        Ok(response)
    }
}
