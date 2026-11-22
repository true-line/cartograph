//! Recovery metrics (#119): per-extractor coverage and the per-ingest
//! history record that makes the determinism invariant observable — two
//! ingests of the same commit must show identical graph content hashes
//! (ADR-0014, AC-0039).

use core_graph::{Edge, Node};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Coverage of one extractor over its scope for a single ingest.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExtractorCoverage {
    /// Extractor id as recorded on provenance (e.g. `t0.adapter-ts`).
    pub extractor: String,
    /// Source files the extractor's scope contained.
    pub files_in_scope: u64,
    /// Distinct files that produced at least one fact.
    pub files_with_facts: u64,
    /// Facts (nodes + edges) attributed to the extractor.
    pub facts: u64,
    /// files_with_facts / files_in_scope, in percent; null when the scope
    /// is unknown (an extractor outside the declared layer map).
    pub coverage_pct: Option<f64>,
}

/// One per-ingest history record: tier tallies, register counts, and the
/// whole-graph content hash.
#[derive(Debug, Clone, Serialize)]
pub struct IngestRecord {
    pub id: i64,
    pub job_id: i64,
    pub repo: String,
    pub commit_sha: String,
    pub confirmed: u64,
    pub inferred_strong: u64,
    pub inferred_weak: u64,
    pub gap: u64,
    pub unsupported: u64,
    pub no_evidence: u64,
    pub graph_facts: u64,
    /// BLAKE3 over the sorted fact identities + their content hashes.
    pub content_hash: String,
    pub created_at: String,
}

/// Tier tallies plus coverage, computed from one graph projection.
pub struct GraphMetrics {
    pub confirmed: u64,
    pub inferred_strong: u64,
    pub inferred_weak: u64,
    pub gap: u64,
    pub graph_facts: u64,
    pub content_hash: String,
    pub coverage: Vec<ExtractorCoverage>,
}

/// Compute tallies, coverage, and the recorded content hash from a
/// whole-graph projection. Tallies describe the whole graph (they must
/// reconcile with findings_summary), while the content hash and coverage
/// both scope to facts whose evidence lives in `coverage_repos` — the repos
/// this ingest actually processed (AC-0060: same commit under the same
/// source registration) — so a multi-repo workspace never changes one
/// registration's recorded hash when a different repo in the workspace
/// changes (#466). A whole-system ingest passes every repo it loaded as
/// `coverage_repos`, so its hash still spans the system it recorded. An
/// empty `coverage_repos` scopes to nothing for coverage (no registration
/// declared) but leaves the hash unscoped, spanning every node/edge given —
/// the whole-graph-hash contract earlier callers with no registration of
/// their own still rely on. `scope` maps extractor id → source files in
/// scope this ingest; extractors it omits (including layers with zero
/// files) report a null coverage_pct. Counts use the spec register's own
/// provenance definition — one definition, every surface.
///
/// `load_into_graph_with_bindings` always publishes the registration's own
/// `repo:<key>` node, whose provenance hashes the commit it was recorded at
/// but carries no evidence spans (it is not itself a piece of extracted
/// evidence). Since that node can never satisfy the evidence-based scope
/// check, it is admitted into the hash by id whenever its key is in
/// `coverage_repos` — otherwise a managed repo with no extracted facts (or
/// whose new commit only touches unextracted files) would record the same
/// scoped hash across different commits (#466 follow-up).
///
/// Production reads the graph packed through [`compute_packed`]; this decoded
/// form is the reference the packed one must equal.
#[cfg(any(test, feature = "test-support"))]
pub fn compute(
    nodes: &[Node],
    edges: &[Edge],
    scope: &BTreeMap<String, u64>,
    coverage_repos: &BTreeSet<String>,
) -> GraphMetrics {
    let mut tally = Tally::new(coverage_repos);
    nodes.iter().for_each(|node| tally.node(node));
    edges.iter().for_each(|edge| tally.edge(edge));
    tally.finish(scope)
}

/// [`compute`] over a packed snapshot: each fact is decoded, tallied and
/// dropped before the next, so recording an ingest never holds the whole
/// graph as props trees (AC-1201). Same metrics, same content hash.
pub fn compute_packed(
    snapshot: &core_graph::packed::PackedSnapshot,
    scope: &BTreeMap<String, u64>,
    coverage_repos: &BTreeSet<String>,
) -> Result<GraphMetrics, core_graph::GraphError> {
    let mut tally = Tally::new(coverage_repos);
    for node in &snapshot.nodes {
        tally.node(&node.unpack()?);
    }
    for edge in &snapshot.edges {
        tally.edge(&edge.unpack()?);
    }
    Ok(tally.finish(scope))
}

/// The running counts behind [`compute`], fed one fact at a time.
struct Tally<'a> {
    coverage_repos: &'a BTreeSet<String>,
    confirmed: u64,
    inferred_strong: u64,
    inferred_weak: u64,
    gap: u64,
    facts: u64,
    facts_by_extractor: BTreeMap<String, u64>,
    files_by_extractor: BTreeMap<String, BTreeSet<String>>,
    // Canonical fact lines, sorted before hashing so store order is
    // irrelevant: identity + per-fact content hash.
    lines: spec::FactLines,
}

impl<'a> Tally<'a> {
    fn new(coverage_repos: &'a BTreeSet<String>) -> Self {
        Self {
            coverage_repos,
            confirmed: 0,
            inferred_strong: 0,
            inferred_weak: 0,
            gap: 0,
            facts: 0,
            facts_by_extractor: BTreeMap::new(),
            files_by_extractor: BTreeMap::new(),
            lines: spec::FactLines::default(),
        }
    }

    // An empty `coverage_repos` means "no registration to scope to" (every
    // production call site passes at least one repo; only whole-graph-hash
    // callers with no registration of their own pass the empty set) — the
    // hash then spans everything given, same as before #466.
    fn in_recorded_scope(&self, provenance: &core_prov::Provenance) -> bool {
        self.coverage_repos.is_empty()
            || provenance
                .evidence
                .iter()
                .any(|evidence| self.coverage_repos.contains(&evidence.repo))
    }

    fn count(&mut self, provenance: &core_prov::Provenance) {
        use core_prov::ConfidenceTier::*;
        self.facts += 1;
        match provenance.confidence_tier {
            Confirmed => self.confirmed += 1,
            InferredStrong => self.inferred_strong += 1,
            InferredWeak => self.inferred_weak += 1,
            Gap => self.gap += 1,
        }
        if !self.in_recorded_scope(provenance) {
            return;
        }
        *self
            .facts_by_extractor
            .entry(provenance.extractor_id.clone())
            .or_default() += 1;
        for evidence in &provenance.evidence {
            if self.coverage_repos.contains(&evidence.repo) {
                self.files_by_extractor
                    .entry(provenance.extractor_id.clone())
                    .or_default()
                    .insert(format!("{}:{}", evidence.repo, evidence.path));
            }
        }
    }

    fn node(&mut self, node: &Node) {
        let provenance = spec::provenance(&node.props, &node.id);
        self.count(&provenance);
        // The registration's own `repo:<key>` node (see `compute`): no
        // evidence spans, but its content hash is bound to the commit it was
        // recorded at, so it must stay in the scoped hash by id.
        let own_registration = node
            .id
            .strip_prefix("repo:")
            .is_some_and(|key| self.coverage_repos.contains(key));
        if self.in_recorded_scope(&provenance) || own_registration {
            self.lines.push_node(node, &provenance);
        }
    }

    fn edge(&mut self, edge: &Edge) {
        let identity = format!("{} {} {}", edge.src, edge.label, edge.dst);
        let provenance = spec::provenance(&edge.props, &identity);
        self.count(&provenance);
        if self.in_recorded_scope(&provenance) {
            self.lines.push_edge(edge, &provenance);
        }
    }

    fn finish(self, scope: &BTreeMap<String, u64>) -> GraphMetrics {
        // One definition of the graph content hash, shared with the export
        // manifest (#1765, AC-0593): sorted lines, so store order is irrelevant.
        let content_hash = self.lines.hash();
        // Scope-declared extractors always appear — a covering adapter that
        // produced zero facts is a 0% row, not a missing row.
        let mut extractors: BTreeSet<String> = scope.keys().cloned().collect();
        extractors.extend(self.facts_by_extractor.keys().cloned());
        let coverage = extractors
            .into_iter()
            .map(|extractor| {
                let files_with_facts = self
                    .files_by_extractor
                    .get(&extractor)
                    .map(|files| files.len() as u64)
                    .unwrap_or(0);
                let files_in_scope = scope.get(&extractor).copied().unwrap_or(0);
                let coverage_pct = if scope.contains_key(&extractor) {
                    Some(if files_in_scope == 0 {
                        0.0
                    } else {
                        (files_with_facts as f64 / files_in_scope as f64) * 100.0
                    })
                } else {
                    None
                };
                ExtractorCoverage {
                    facts: self
                        .facts_by_extractor
                        .get(&extractor)
                        .copied()
                        .unwrap_or(0),
                    files_with_facts,
                    files_in_scope,
                    coverage_pct,
                    extractor,
                }
            })
            .collect();
        GraphMetrics {
            confirmed: self.confirmed,
            inferred_strong: self.inferred_strong,
            inferred_weak: self.inferred_weak,
            gap: self.gap,
            graph_facts: self.facts,
            content_hash,
            coverage,
        }
    }
}

/// Store for ingest history + coverage, backed by the state-spine database.
pub struct MetricsStore {
    conn: Connection,
}

impl MetricsStore {
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS ingest_history (
                 id           INTEGER PRIMARY KEY AUTOINCREMENT,
                 job_id       INTEGER NOT NULL,
                 repo         TEXT NOT NULL,
                 commit_sha   TEXT NOT NULL,
                 confirmed    INTEGER NOT NULL,
                 inferred_strong INTEGER NOT NULL,
                 inferred_weak INTEGER NOT NULL,
                 gap          INTEGER NOT NULL,
                 unsupported  INTEGER NOT NULL,
                 no_evidence  INTEGER NOT NULL,
                 graph_facts  INTEGER NOT NULL,
                 content_hash TEXT NOT NULL,
                 created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             );
             CREATE TABLE IF NOT EXISTS extractor_coverage (
                 history_id      INTEGER NOT NULL REFERENCES ingest_history(id),
                 extractor       TEXT NOT NULL,
                 files_in_scope  INTEGER NOT NULL,
                 files_with_facts INTEGER NOT NULL,
                 facts           INTEGER NOT NULL,
                 scoped          INTEGER NOT NULL CHECK (scoped IN (0, 1)),
                 PRIMARY KEY (history_id, extractor)
             );",
        )?;
        Ok(Self { conn })
    }

    /// Persist one ingest's metrics; returns the stored record.
    pub fn record(
        &mut self,
        job_id: i64,
        repo: &str,
        commit_sha: &str,
        metrics: &GraphMetrics,
        unsupported: u64,
        no_evidence: u64,
    ) -> rusqlite::Result<IngestRecord> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO ingest_history
               (job_id, repo, commit_sha, confirmed, inferred_strong, inferred_weak,
                gap, unsupported, no_evidence, graph_facts, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                job_id,
                repo,
                commit_sha,
                metrics.confirmed as i64,
                metrics.inferred_strong as i64,
                metrics.inferred_weak as i64,
                metrics.gap as i64,
                unsupported as i64,
                no_evidence as i64,
                metrics.graph_facts as i64,
                metrics.content_hash,
            ],
        )?;
        let history_id = tx.last_insert_rowid();
        for coverage in &metrics.coverage {
            tx.execute(
                "INSERT INTO extractor_coverage
                   (history_id, extractor, files_in_scope, files_with_facts, facts, scoped)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    history_id,
                    coverage.extractor,
                    coverage.files_in_scope as i64,
                    coverage.files_with_facts as i64,
                    coverage.facts as i64,
                    coverage.coverage_pct.is_some(),
                ],
            )?;
        }
        tx.commit()?;
        let record = self
            .history(1)?
            .into_iter()
            .next()
            .expect("row just inserted");
        Ok(record)
    }

    /// Ingest records, newest first.
    pub fn history(&self, limit: u32) -> rusqlite::Result<Vec<IngestRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, job_id, repo, commit_sha, confirmed, inferred_strong,
                    inferred_weak, gap, unsupported, no_evidence, graph_facts,
                    content_hash, created_at
             FROM ingest_history ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(IngestRecord {
                id: row.get(0)?,
                job_id: row.get(1)?,
                repo: row.get(2)?,
                commit_sha: row.get(3)?,
                confirmed: row.get::<_, i64>(4)? as u64,
                inferred_strong: row.get::<_, i64>(5)? as u64,
                inferred_weak: row.get::<_, i64>(6)? as u64,
                gap: row.get::<_, i64>(7)? as u64,
                unsupported: row.get::<_, i64>(8)? as u64,
                no_evidence: row.get::<_, i64>(9)? as u64,
                graph_facts: row.get::<_, i64>(10)? as u64,
                content_hash: row.get(11)?,
                created_at: row.get(12)?,
            })
        })?;
        rows.collect()
    }

    /// The graph content hash recorded by the latest completed ingest of
    /// `repo` at `commit` (#1765, AC-0593), or `None` when no ingest of that
    /// registration at that revision has been recorded — a whole-system
    /// ingest records one hash for the system, not one per registration.
    pub fn recorded_content_hash(
        &self,
        repo: &str,
        commit: &str,
    ) -> rusqlite::Result<Option<String>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT content_hash FROM ingest_history
                 WHERE repo = ?1 AND commit_sha = ?2 ORDER BY id DESC LIMIT 1",
                params![repo, commit],
                |row| row.get(0),
            )
            .optional()
    }

    /// Coverage rows for the most recent ingest (empty before the first).
    pub fn latest_coverage(&self) -> rusqlite::Result<Vec<ExtractorCoverage>> {
        let mut stmt = self.conn.prepare(
            "SELECT extractor, files_in_scope, files_with_facts, facts, scoped
             FROM extractor_coverage
             WHERE history_id = (SELECT MAX(id) FROM ingest_history)
             ORDER BY extractor",
        )?;
        let rows = stmt.query_map([], |row| {
            let files_in_scope = row.get::<_, i64>(1)? as u64;
            let files_with_facts = row.get::<_, i64>(2)? as u64;
            let scoped: bool = row.get(4)?;
            Ok(ExtractorCoverage {
                extractor: row.get(0)?,
                files_in_scope,
                files_with_facts,
                facts: row.get::<_, i64>(3)? as u64,
                coverage_pct: scoped.then(|| {
                    if files_in_scope == 0 {
                        0.0
                    } else {
                        (files_with_facts as f64 / files_in_scope as f64) * 100.0
                    }
                }),
            })
        })?;
        rows.collect()
    }
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use super::*;
    use serde_json::json;

    fn fact_props(extractor: &str, confidence: &str, path: &str, hash: &str) -> serde_json::Value {
        json!({
            "prov": {
                "tier": "Deterministic",
                "confidence_tier": confidence,
                "evidence": [{
                    "repo": "local/fixture",
                    "path": path,
                    "byte_start": 0,
                    "byte_end": 10,
                    "commit_sha": "workdir",
                }],
                "extractor_id": extractor,
                "content_hash": hash,
            }
        })
    }

    fn node(id: &str, extractor: &str, confidence: &str, path: &str) -> Node {
        Node {
            id: id.into(),
            label: "Symbol".into(),
            props: fact_props(extractor, confidence, path, &"a".repeat(64)),
        }
    }

    fn edge(src: &str, dst: &str, extractor: &str) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: "CALLS".into(),
            props: fact_props(extractor, "Confirmed", "src/a.ts", &"b".repeat(64)),
        }
    }

    fn ts_scope(files: u64) -> BTreeMap<String, u64> {
        BTreeMap::from([("t0.adapter-ts".to_string(), files)])
    }

    fn fixture_repos() -> BTreeSet<String> {
        BTreeSet::from(["local/fixture".to_string()])
    }

    #[test]
    fn coverage_counts_distinct_files_and_reports_unscoped_extractors() {
        let nodes = vec![
            node("a", "t0.adapter-ts", "Confirmed", "src/a.ts"),
            node("b", "t0.adapter-ts", "Confirmed", "src/a.ts"),
            node("c", "t0.adapter-ts", "Confirmed", "src/c.ts"),
            node("d", "t1.dynamic", "InferredStrong", "traces/run.json"),
        ];
        let edges = vec![edge("a", "b", "t0.adapter-ts")];
        let metrics = compute(&nodes, &edges, &ts_scope(4), &fixture_repos());

        let ts = metrics
            .coverage
            .iter()
            .find(|c| c.extractor == "t0.adapter-ts")
            .unwrap();
        // Two facts in a.ts count one file; the edge shares a.ts too.
        assert_eq!(ts.files_with_facts, 2);
        assert_eq!(ts.facts, 4);
        assert_eq!(ts.coverage_pct, Some(50.0));

        // An extractor outside the declared scope is visible but unmeasured.
        let dynamic = metrics
            .coverage
            .iter()
            .find(|c| c.extractor == "t1.dynamic")
            .unwrap();
        assert_eq!(dynamic.facts, 1);
        assert_eq!(dynamic.coverage_pct, None);

        assert_eq!(metrics.confirmed, 4);
        assert_eq!(metrics.inferred_strong, 1);
        assert_eq!(metrics.graph_facts, 5);
    }

    #[test]
    // AC-1201/T-1201: metrics over a packed snapshot, decoded one fact at a
    // time, equal the metrics over the decoded graph: tallies, coverage and
    // the scoped content hash, foreign, registration and fallback facts included.
    fn packed_metrics_equal_decoded_metrics() {
        let mut foreign = node("z", "t0.adapter-ts", "Confirmed", "src/old.ts");
        foreign.props["prov"]["evidence"][0]["repo"] = json!("local/previous");
        let unproven = Node {
            id: "loose".into(),
            label: "Symbol".into(),
            props: json!({ "name": "loose" }),
        };
        let registration = Node {
            id: "repo:local/fixture".into(),
            label: "Repo".into(),
            props: json!({ "commit": "abc" }),
        };
        let nodes = vec![
            node("a", "t0.adapter-ts", "Confirmed", "src/a.ts"),
            node("d", "t1.dynamic", "InferredStrong", "traces/run.json"),
            foreign,
            unproven,
            registration,
        ];
        let edges = vec![
            edge("a", "d", "t0.adapter-ts"),
            edge("a", "z", "t1.dynamic"),
        ];
        let snapshot = core_graph::packed::PackedSnapshot {
            nodes: nodes
                .iter()
                .cloned()
                .map(|node| core_graph::packed::PackedNode::pack(node).unwrap())
                .collect(),
            edges: edges
                .iter()
                .cloned()
                .map(|edge| core_graph::packed::PackedEdge::pack(edge).unwrap())
                .collect(),
        };
        for repos in [fixture_repos(), BTreeSet::new()] {
            let decoded = compute(&nodes, &edges, &ts_scope(3), &repos);
            let packed = compute_packed(&snapshot, &ts_scope(3), &repos).unwrap();
            assert_eq!(packed.content_hash, decoded.content_hash);
            assert_eq!(packed.coverage, decoded.coverage);
            assert_eq!(
                (
                    packed.confirmed,
                    packed.inferred_strong,
                    packed.inferred_weak
                ),
                (
                    decoded.confirmed,
                    decoded.inferred_strong,
                    decoded.inferred_weak
                )
            );
            assert_eq!(
                (packed.gap, packed.graph_facts),
                (decoded.gap, decoded.graph_facts)
            );
            assert_eq!(packed.graph_facts, 7);
        }
    }

    #[test]
    fn coverage_attributes_only_the_current_ingests_repos() {
        // A prior ingest's facts stay in the graph — tallies describe the
        // whole graph, but coverage and the content hash must not mix runs
        // (#138 review, #466): otherwise a 1-file re-ingest against a
        // 100-file graph reports >100% coverage.
        let mut foreign = node("z", "t0.adapter-ts", "Confirmed", "src/old.ts");
        foreign.props["prov"]["evidence"][0]["repo"] = json!("local/previous");
        let nodes = vec![node("a", "t0.adapter-ts", "Confirmed", "src/a.ts"), foreign];
        let metrics = compute(&nodes, &[], &ts_scope(1), &fixture_repos());

        let ts = &metrics.coverage[0];
        assert_eq!(ts.facts, 1);
        assert_eq!(ts.files_with_facts, 1);
        assert_eq!(ts.coverage_pct, Some(100.0));
        // The whole-graph tallies still see both facts...
        assert_eq!(metrics.graph_facts, 2);
        assert_eq!(metrics.confirmed, 2);
    }

    #[test]
    // AC-0060/T-0060: a registration's recorded hash does not change when an
    // unrelated repo in the same multi-repo workspace changes (#466) — only
    // the tallies span the whole graph; the recorded hash scopes to
    // `coverage_repos`, matching AC-0060's "same commit under the same
    // source registration".
    fn content_hash_is_scoped_to_the_recorded_repos_not_the_whole_graph() {
        let own = node("a", "t0.adapter-ts", "Confirmed", "src/a.ts");
        let mut other_repo = node("z", "t0.adapter-ts", "Confirmed", "src/z.ts");
        other_repo.props["prov"]["evidence"][0]["repo"] = json!("local/other");
        let scope = ts_scope(1);

        let before = compute(
            &[own.clone(), other_repo.clone()],
            &[],
            &scope,
            &fixture_repos(),
        );

        // The other repo's facts change (a different commit of `local/other`)
        // while `local/fixture`'s own facts are untouched.
        let mut other_repo_changed = other_repo.clone();
        other_repo_changed.props["prov"]["content_hash"] = json!("c".repeat(64));
        let after = compute(&[own, other_repo_changed], &[], &scope, &fixture_repos());

        assert_eq!(before.content_hash, after.content_hash);
        // Tallies still reconcile with findings_summary over the whole graph.
        assert_eq!(before.graph_facts, 2);
        assert_eq!(after.graph_facts, 2);
    }

    // Mirrors `load_into_graph_with_bindings`'s registration node: no
    // evidence spans, but a content hash bound to `commit`.
    fn registration_node(repo: &str, commit: &str) -> Node {
        let hash = core_prov::content_hash(
            &serde_json::to_vec(&("registered-repo-v1", repo, commit)).unwrap(),
        );
        Node {
            id: format!("repo:{repo}"),
            label: "Repo".into(),
            props: json!({
                "commit": commit,
                "prov": {
                    "tier": "Deterministic",
                    "confidence_tier": "Confirmed",
                    "evidence": [],
                    "extractor_id": "app.ingest",
                    "content_hash": hash,
                }
            }),
        }
    }

    #[test]
    // Codex review on #518: the registration's own `repo:<key>` node has no
    // evidence, so it used to be silently dropped from the scoped hash — a
    // managed repo with zero extracted facts recorded the same hash across
    // different commits. The node must move the hash even with no other
    // facts in scope.
    fn zero_evidence_registration_node_moves_the_scoped_hash() {
        let before = compute(
            &[registration_node("local/fixture", "c1")],
            &[],
            &BTreeMap::new(),
            &fixture_repos(),
        );
        let after = compute(
            &[registration_node("local/fixture", "c2")],
            &[],
            &BTreeMap::new(),
            &fixture_repos(),
        );
        assert_ne!(before.content_hash, after.content_hash);

        // A foreign registration's commit must not move it (no #466 regression).
        let with_foreign_c1 = compute(
            &[
                registration_node("local/fixture", "c1"),
                registration_node("local/other", "x1"),
            ],
            &[],
            &BTreeMap::new(),
            &fixture_repos(),
        );
        let with_foreign_c2 = compute(
            &[
                registration_node("local/fixture", "c1"),
                registration_node("local/other", "x2"),
            ],
            &[],
            &BTreeMap::new(),
            &fixture_repos(),
        );
        assert_eq!(with_foreign_c1.content_hash, with_foreign_c2.content_hash);
    }

    #[test]
    fn scoped_adapter_with_zero_facts_is_a_zero_row_not_a_missing_row() {
        let metrics = compute(&[], &[], &ts_scope(3), &fixture_repos());
        let ts = &metrics.coverage[0];
        assert_eq!(ts.extractor, "t0.adapter-ts");
        assert_eq!(ts.coverage_pct, Some(0.0));
    }

    #[test]
    fn content_hash_is_order_independent_and_content_sensitive() {
        let a = node("a", "t0.adapter-ts", "Confirmed", "src/a.ts");
        let b = node("b", "t0.adapter-ts", "Confirmed", "src/b.ts");
        let scope = ts_scope(2);

        let forward = compute(&[a.clone(), b.clone()], &[], &scope, &fixture_repos());
        let reversed = compute(&[b.clone(), a.clone()], &[], &scope, &fixture_repos());
        // AC-0039 made observable: same facts ⇒ same hash, store order aside.
        assert_eq!(forward.content_hash, reversed.content_hash);

        let mut changed = a.clone();
        changed.props["prov"]["content_hash"] = json!("c".repeat(64));
        let different = compute(&[changed, b], &[], &scope, &fixture_repos());
        assert_ne!(forward.content_hash, different.content_hash);
    }

    #[test]
    fn history_round_trips_and_orders_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MetricsStore::open(dir.path().join("state.db")).unwrap();
        let nodes = vec![node("a", "t0.adapter-ts", "Confirmed", "src/a.ts")];
        let metrics = compute(&nodes, &[], &ts_scope(1), &fixture_repos());

        let first = store
            .record(7, "local/fixture", "workdir", &metrics, 2, 1)
            .unwrap();
        assert_eq!(first.unsupported, 2);
        assert_eq!(first.no_evidence, 1);
        assert_eq!(first.graph_facts, 1);
        assert_eq!(first.content_hash, metrics.content_hash);

        // A second identical ingest records the identical hash — the
        // determinism invariant as data, not just a test assertion.
        let second = store
            .record(8, "local/fixture", "workdir", &metrics, 2, 1)
            .unwrap();
        let history = store.history(10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].id, second.id);
        assert_eq!(history[0].content_hash, history[1].content_hash);

        let coverage = store.latest_coverage().unwrap();
        assert_eq!(coverage.len(), 1);
        assert_eq!(coverage[0].coverage_pct, Some(100.0));
    }

    // AC-0593: the recorded hash and the export manifest's hash are one
    // definition — an unscoped record equals the whole-graph content hash.
    #[test]
    fn unscoped_recorded_hash_equals_the_whole_graph_content_hash() {
        let mut foreign = node("z", "t0.adapter-ts", "Confirmed", "src/z.ts");
        foreign.props["prov"]["evidence"][0]["repo"] = json!("local/other");
        let nodes = vec![node("a", "t0.adapter-ts", "Confirmed", "src/a.ts"), foreign];
        let edges = vec![edge("a", "z", "t0.adapter-ts")];

        let unscoped = compute(&nodes, &edges, &BTreeMap::new(), &BTreeSet::new());
        assert_eq!(
            unscoped.content_hash,
            spec::graph_content_hash(&nodes, &edges)
        );
        // A registration's own record covers only its facts.
        let scoped = compute(&nodes, &edges, &BTreeMap::new(), &fixture_repos());
        assert_ne!(scoped.content_hash, unscoped.content_hash);
    }

    // AC-0593: the hash shown with a source registration is the latest one
    // recorded for that registration at that revision, and no other.
    #[test]
    fn recorded_content_hash_is_per_registration_and_revision() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MetricsStore::open(dir.path().join("state.db")).unwrap();
        let hash = |id: &str| {
            compute(
                &[node(id, "t0.adapter-ts", "Confirmed", "src/a.ts")],
                &[],
                &ts_scope(1),
                &fixture_repos(),
            )
        };
        let (old, new, other) = (hash("old"), hash("new"), hash("other"));
        store
            .record(1, "local/fixture", "workdir", &old, 0, 0)
            .unwrap();
        store
            .record(2, "local/fixture", "workdir", &new, 0, 0)
            .unwrap();
        store
            .record(3, "acme/shop", "abc123", &other, 0, 0)
            .unwrap();
        store
            .record(4, "local/fixture,acme/shop", "system", &other, 0, 0)
            .unwrap();

        let recorded =
            |repo: &str, commit: &str| store.recorded_content_hash(repo, commit).unwrap();
        assert_eq!(recorded("local/fixture", "workdir"), Some(new.content_hash));
        assert_eq!(recorded("acme/shop", "abc123"), Some(other.content_hash));
        assert_eq!(recorded("acme/shop", "def456"), None);
        assert_eq!(recorded("local/unknown", "workdir"), None);
    }
}
