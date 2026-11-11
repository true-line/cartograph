//! Metadata-only catalogue, admitted before aggregate report allocation.
use super::*;
use adapters_lang_ts::captured::Receipt;
use core_graph::{SnapshotReadLimits, coverage::CoverageReadLimits, source::*};
use std::io::Write;

const MAX_CANDIDATES: usize = 4096;
const MAX_CANDIDATE_BYTES: usize = 8 * 1024 * 1024;
const MAX_REPORT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Serialize)]
pub(in super::super) struct Candidate {
    pub(in super::super) binding: SourceBinding,
    label: String,
    provenance: Option<core_prov::Provenance>,
    provenance_status: &'static str,
    /// Exact receipt ranges supply true role/index and ordered viewer ordinal.
    /// No source text or caller-selected target is included.
    pub(in super::super) receipt: Receipt,
}

#[derive(Serialize)]
pub(in super::super) struct GraphPreparation {
    pub graph_snapshot_id: String,
    pub source_id: String,
    pub repo_key: String,
    pub capture_id: String,
    pub capture: source_capture::CaptureManifest,
    pub coverage: core_graph::coverage::StoredCoverage,
    pub(in super::super) candidates: Vec<Candidate>,
    pub facts_without_primary_binding: usize,
    input_closure: &'static str,
    selection_status: &'static str,
}

struct Count(usize);
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other("preparation byte limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn measure(value: &impl Serialize, maximum: usize) -> Result<usize, String> {
    let mut count = Count(maximum);
    serde_json::to_writer(&mut count, value).map_err(|_| "preparation byte limit")?;
    Ok(maximum - count.0)
}

pub(super) fn write_new(path: &Path, value: &impl Serialize) -> Result<(), String> {
    // Admission uses the same compact serializer as the final writer. No large
    // intermediate Value/String/Vec is needed to count or persist the report.
    measure(value, MAX_REPORT_BYTES)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| "report must be new and writable")?;
    serde_json::to_writer(&mut file, value).map_err(|_| "cannot write preparation report")?;
    file.sync_all()
        .map_err(|_| "cannot sync preparation report".into())
}

fn captured_input(
    state: &AppState,
    source: &RegisteredSource,
    input: &input::VerifiedInput,
    data: &Path,
) -> Result<(String, source_capture::CaptureManifest), String> {
    let inventory = crate::primary_source::preview(state, &source.source_id)?;
    let expected_bytes: u64 = input.files.values().map(|file| file.byte_len).sum();
    if inventory.captures != 1
        || inventory.files != input.files.len() as u64
        || inventory.bytes != expected_bytes
    {
        return Err("capture membership differs from verified input".into());
    }
    // The production store path is shared, never copied/rebased. A source guard
    // is held by collect while this second read-only operation validates every
    // object using the existing CaptureStore implementation.
    let store = source_capture::CaptureStore::open(
        &data.join("retained-source/captures.sqlite"),
        source_capture::StoreLimits::default(),
    )
    .map_err(|_| "cannot open retained capture")?;
    let capture = store
        .load(&inventory.capture_ids[0])
        .map_err(|_| "retained capture is invalid")?;
    if capture.manifest().source_id.as_str() != source.source_id
        || !matches!(
            capture.manifest().kind,
            source_capture::CaptureKind::WorkingTree
        )
    {
        return Err("capture source or working-tree identity differs".into());
    }
    if capture.manifest().files.len() != input.files.len() {
        return Err("capture membership differs from verified input".into());
    }
    for file in &capture.manifest().files {
        let expected = input
            .files
            .get(&file.path)
            .ok_or("unexpected captured file")?;
        if file.digest != expected.blake3 || file.byte_len != expected.byte_len {
            return Err("captured bytes differ from original input proof".into());
        }
    }
    Ok((capture.id().to_owned(), capture.manifest().clone()))
}

pub(super) fn collect(
    state: &AppState,
    source: &RegisteredSource,
    input: &input::VerifiedInput,
    data: &Path,
) -> Result<GraphPreparation, String> {
    let guard = state
        .primary_sources
        .task_guard(&source.source_id)
        .map_err(|_| "capture guard unavailable")?;
    let (capture_id, capture) = captured_input(state, source, input, data)?;
    let graph = state.graph.lock().map_err(|_| "graph unavailable")?;
    let ((nodes, edges), mut coverage) = graph
        .read_snapshot_with_coverage_bounded(
            SnapshotReadLimits::default(),
            CoverageReadLimits {
                max_records: 1,
                max_bytes: 2 * 1024 * 1024,
            },
        )
        .map_err(|_| "bounded graph and coverage unavailable")?;
    if coverage.len() != 1 {
        return Err("expected one production coverage record".into());
    }
    let coverage = coverage.remove(0);
    if coverage.record.repo_key != source.repo_key
        || coverage.record.registered_source_id.as_deref() != Some(&source.source_id)
        || coverage.record.config_hash
            != crate::coverage_config_hash(&[], crate::Registries::builtin())
    {
        return Err("production coverage basis differs".into());
    }
    let mut candidates = Vec::new();
    let mut remaining = MAX_CANDIDATE_BYTES - 2; // complete array brackets
    let mut unbound = 0;
    let keys = nodes
        .iter()
        .map(|node| {
            (
                FactKey::from_node(node),
                node_digest(node),
                &node.label,
                &node.props,
            )
        })
        .chain(edges.iter().map(|edge| {
            (
                FactKey::from_edge(edge),
                edge_digest(edge),
                &edge.label,
                &edge.props,
            )
        }));
    for (fact, digest, label, props) in keys {
        let Some(binding) = graph
            .current_source_binding(&fact)
            .map_err(|_| "binding unavailable")?
        else {
            unbound += 1;
            continue;
        };
        if binding.repo_key != source.repo_key
            || digest.map_err(|_| "invalid fact digest")? != binding.emitted_fact_digest
        {
            return Err("primary binding differs from recovered fact".into());
        }
        if candidates.len() == MAX_CANDIDATES {
            return Err("candidate count limit".into());
        }
        let receipt = state
            .primary_sources
            .task_receipt(
                &guard,
                &fact,
                &binding.emitted_fact_digest,
                &source.repo_key,
                &binding.receipt_id,
            )
            .map_err(|_| "invalid retained receipt")?;
        let file = receipt.file();
        if file.capture_id != capture_id
            || input.files.get(&file.path).is_none_or(|expected| {
                expected.blake3 != file.digest || expected.byte_len != file.byte_len
            })
        {
            return Err("candidate receipt is outside the frozen capture".into());
        }
        let provenance = props.get("prov").and_then(|value| {
            let provenance: core_prov::Provenance = serde_json::from_value(value.clone()).ok()?;
            provenance.validate().ok()?;
            Some(provenance)
        });
        let provenance_status = if provenance.is_some() {
            "validated"
        } else {
            "missing_or_invalid"
        };
        let candidate = Candidate {
            binding,
            label: label.clone(),
            provenance,
            provenance_status,
            receipt,
        };
        if !candidates.is_empty() {
            remaining = remaining.checked_sub(1).ok_or("candidate byte limit")?;
        }
        remaining -= measure(&candidate, remaining)?;
        candidates.push(candidate);
    }
    // Snapshot ownership is transferred once; there is no whole-graph clone or
    // source-specific synthetic inventory. Catalogue order is typed-key order.
    candidates.sort_by(|a, b| a.binding.fact.cmp(&b.binding.fact));
    let graph_snapshot_id = context_hub::ContextSnapshot::new(nodes, edges)
        .map_err(|_| "invalid recovered snapshot")?
        .id()
        .to_owned();
    Ok(GraphPreparation {
        graph_snapshot_id,
        source_id: source.source_id.clone(),
        repo_key: source.repo_key.clone(),
        capture_id,
        capture,
        coverage,
        candidates,
        facts_without_primary_binding: unbound,
        input_closure: "input_closure_not_established",
        selection_status: "caller_selection_required",
    })
}
