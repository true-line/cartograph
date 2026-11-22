//! The stores the Tauri runtime manages, split out of `main.rs` so code
//! that reads them can compile outside the app crate (#1924). The app
//! imports it at its crate root, so `crate::AppState` is unchanged.
use crate::findings::FindingStore;
use crate::job_execution::JobExecutionLocks;
use crate::jobs::JobStore;
use crate::sources::SourceRegistry;
use core_graph::SqliteGraphStore;
use std::sync::{Arc, Mutex};

/// Stores managed by the Tauri runtime. Graph and state spine are separate
/// databases (ADR-0008): the graph is a disposable ingest artifact, the spine
/// holds durable state.
pub struct AppState {
    pub graph: Mutex<SqliteGraphStore>,
    pub jobs: Mutex<JobStore>,
    pub job_execution_locks: JobExecutionLocks,
    pub investigations: crate::investigation_runtime::InvestigationRuntime,
    pub findings: Mutex<FindingStore>,
    pub settings: Mutex<crate::settings::SettingsStore>,
    pub decisions: Mutex<agents::DecisionLog>,
    pub proposals: Mutex<agents::ProposalStore>,
    /// Section drafts and their reviews (SPEC-15, #1748).
    pub drafts: Mutex<agents::draft_store::DraftStore>,
    pub work_packets: crate::packet_runtime::PacketRuntime,
    pub extraction_caches: Mutex<crate::extraction_cache::ExtractionCaches>,
    pub embedding_cache: Option<Arc<Mutex<semantic::embedding_cache::EmbeddingCache>>>,
    /// ADR-0051 knowledge overlay, shared with the graph reset hook.
    pub knowledge_overlay: Option<Arc<Mutex<core_graph::overlay::OverlayStore>>>,
    /// Durable, host-owned authority for operational source locations.
    pub sources: Arc<Mutex<SourceRegistry>>,
    pub primary_sources: crate::primary_source::PrimarySourceStore,
    pub metrics: Mutex<crate::metrics::MetricsStore>,
    /// In memory only: what an export's second compile settled for the
    /// current graph revision (`curated::DerivedSettlements`, #1834).
    pub curated_settled: Arc<crate::curated_settlements::DerivedSettlements>,
}
