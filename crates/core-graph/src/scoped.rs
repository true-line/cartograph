//! Bounded keyset acquisitions, explicitly not complete graph snapshots.
//!
//! Row raw/canonical/depth/value budgets reuse SnapshotReadLimits. Revision and
//! schema validation workspace, request/continuation keys, and result envelopes
//! are separate from canonical graph-row bytes. Keys are capped by max_bytes
//! before SQL; callers must also budget their complete transport response.
use crate::{
    Edge, GenerationError, GraphError, Node, SnapshotBoundsError, SnapshotReadLimits,
    SqliteGraphStore, StoreRevision,
    bounded::{self, JsonBudget},
    generation::read_store_revision,
    source::{self, FactKey, FactSourceSelection, FactSourceState, SelectionBinding},
};
use rusqlite::{Connection, params_from_iter, types::Value as SqlValue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// SQLite edge primary-key ordering, deliberately distinct from FactKey order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeKey {
    /// Source identity, ordered first using SQLite BINARY collation.
    pub src: String,
    /// Destination identity, ordered second.
    pub dst: String,
    /// Relation label, ordered third.
    pub label: String,
}
/// Explicit node-window inputs; None after_id starts at the first primary key.
#[derive(Debug, Clone)]
pub struct NodeWindowRequest {
    /// Required for continuation; omitted only for an initial acquisition.
    pub expected_revision: Option<StoreRevision>,
    /// Exclusive lower bound, not an offset or a graph identity.
    pub after_id: Option<String>,
    /// Narrowable existing hard graph budgets, applied to this window only.
    pub limits: SnapshotReadLimits,
}
/// Explicit edge-window inputs in (src,dst,label) primary-key order.
#[derive(Debug, Clone)]
pub struct EdgeWindowRequest {
    /// Required for continuation; omitted only for an initial acquisition.
    pub expected_revision: Option<StoreRevision>,
    /// Exclusive composite lower bound.
    pub after_key: Option<EdgeKey>,
    /// Narrowable existing hard graph budgets, applied to this window only.
    pub limits: SnapshotReadLimits,
}
/// Whether raw storage contains another row; no exact remaining count is claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Remaining {
    /// At least one later raw row exists, not necessarily valid or visible.
    More,
    /// No later raw row exists at this revision; not semantic/global absence.
    Exhausted,
}
/// A complete admitted raw window, not a projection or whole-system claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactWindow<T, K> {
    /// Coherent database instance/generation acquired with these rows.
    pub revision: StoreRevision,
    /// All rows in this admitted keyset window.
    pub rows: Vec<T>,
    /// Raw rows decoded; existence-only lookahead is not counted as a decoded row.
    pub scanned_raw_rows: usize,
    /// Aggregate stored UTF-8 field bytes before decoding.
    pub raw_bytes: usize,
    /// Canonical complete-graph tuple encoding of these rows only.
    pub canonical_bytes: usize,
    /// Decoded JSON values including envelopes and duplicate input properties.
    pub json_values: usize,
    /// Last delivered key when More; None when Exhausted.
    pub next_key: Option<K>,
    /// Explicit remaining raw coverage at this revision.
    pub remaining: Remaining,
}
/// Exact selected facts and per-key source states, never a complete graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedFacts {
    /// Coherent revision covering facts and source associations.
    pub revision: StoreRevision,
    /// Requested present nodes, ordered by primary key.
    pub nodes: Vec<Node>,
    /// Requested present edges, ordered by (src,dst,label).
    pub edges: Vec<Edge>,
    /// Unique requested keys in FactKey order, including missing/invalid bindings.
    pub selections: Vec<FactSourceSelection>,
    /// Aggregate stored fact UTF-8 bytes, excluding bounded binding metadata.
    pub raw_bytes: usize,
    /// Canonical graph-row bytes, excluding selection/revision envelopes.
    pub canonical_bytes: usize,
}

#[derive(Clone, Copy)]
enum Kind {
    Node,
    Edge,
}
impl Kind {
    fn table(self) -> &'static str {
        match self {
            Self::Node => "nodes",
            Self::Edge => "edges",
        }
    }
    fn columns(self) -> &'static [&'static str] {
        match self {
            Self::Node => &["id", "label", "props"],
            Self::Edge => &["src", "dst", "label", "props"],
        }
    }
}

/// Keyset comparisons must use the declared BINARY ascending primary key.
/// Validate PRAGMA metadata before copying even its bounded index name.
fn preflight_schema(connection: &Connection) -> Result<(), GraphError> {
    bounded::preflight_schema(connection)?;
    for (table, columns) in [
        ("nodes", &["id"][..]),
        ("edges", &["src", "dst", "label"][..]),
    ] {
        let valid: bool = connection.query_row("SELECT count(*)=1 AND coalesce(min(typeof(name)='text' AND length(CAST(name AS BLOB)) BETWEEN 1 AND 256),0) FROM pragma_index_list(?1) WHERE origin='pk'",[table],|r|r.get(0))?;
        if !valid {
            return Err(SnapshotBoundsError::InvalidSchema.into());
        }
        let index: String = connection.query_row(
            "SELECT name FROM pragma_index_list(?1) WHERE origin='pk'",
            [table],
            |r| r.get(0),
        )?;
        let count: i64 = connection.query_row(
            "SELECT count(*) FROM pragma_index_xinfo(?1) WHERE key=1",
            [&index],
            |r| r.get(0),
        )?;
        if count != columns.len() as i64 {
            return Err(SnapshotBoundsError::InvalidSchema.into());
        }
        for (position, column) in columns.iter().enumerate() {
            let valid: bool = connection.query_row("SELECT count(*)=1 AND coalesce(min(name=?2 AND coll='BINARY' AND desc=0 AND key=1 AND cid>=0),0) FROM pragma_index_xinfo(?1) WHERE seqno=?3",rusqlite::params![index,column,position as i64],|r|r.get(0))?;
            if !valid {
                return Err(SnapshotBoundsError::InvalidSchema.into());
            }
        }
    }
    Ok(())
}

fn validate_request(
    limits: SnapshotReadLimits,
    expected: Option<&StoreRevision>,
    continuation: bool,
    key_parts: &[&str],
) -> Result<(), GraphError> {
    limits.validate()?;
    if limits.max_rows == 0 || (continuation && expected.is_none()) {
        return Err(SnapshotBoundsError::InvalidLimits.into());
    }
    let mut key_bytes = 0usize;
    for key in key_parts {
        key_bytes = key_bytes
            .checked_add(key.len())
            .ok_or(SnapshotBoundsError::RawByteLimit)?;
    }
    if key_bytes > limits.max_bytes {
        return Err(SnapshotBoundsError::RawByteLimit.into());
    }
    Ok(())
}
fn check_revision(
    connection: &rusqlite::Transaction<'_>,
    expected: Option<&StoreRevision>,
) -> Result<StoreRevision, GraphError> {
    let revision = read_store_revision(connection)?;
    if expected.is_some_and(|value| value != &revision) {
        return Err(GenerationError::Changed.into());
    }
    Ok(revision)
}
fn budget(limits: SnapshotReadLimits) -> Result<JsonBudget, GraphError> {
    let mut budget = JsonBudget::new(limits);
    budget.enter(1)?;
    budget.enter(2)?;
    budget.enter(2)?;
    Ok(budget)
}

/// SQL preflight visits only the same bounded primary-key slice as the body read.
fn read_rows(
    connection: &Connection,
    kind: Kind,
    suffix: &str,
    params: &[SqlValue],
    limits: SnapshotReadLimits,
    budget: &mut JsonBudget,
    raw_remaining: &mut usize,
) -> Result<(Vec<Node>, Vec<Edge>), GraphError> {
    let columns = kind.columns();
    let projection = columns
        .iter()
        .map(|c| format!("typeof({c})='text',length(CAST({c} AS BLOB))"))
        .collect::<Vec<_>>()
        .join(",");
    let mut preflight = connection.prepare(&format!(
        "SELECT {projection} FROM {} {suffix}",
        kind.table()
    ))?;
    let mut metadata = preflight.query(params_from_iter(params))?;
    let mut count = 0usize;
    while let Some(row) = metadata.next()? {
        count += 1;
        if count > limits.max_rows {
            return Err(SnapshotBoundsError::RowLimit.into());
        }
        for i in 0..columns.len() {
            if !row.get::<_, bool>(i * 2)? {
                return Err(SnapshotBoundsError::InvalidRow.into());
            }
            let length = row
                .get::<_, Option<i64>>(i * 2 + 1)?
                .and_then(|v| usize::try_from(v).ok())
                .ok_or(SnapshotBoundsError::InvalidRow)?;
            *raw_remaining = raw_remaining
                .checked_sub(length)
                .ok_or(SnapshotBoundsError::RawByteLimit)?;
        }
    }
    drop(metadata);
    drop(preflight);
    let mut statement = connection.prepare(&format!(
        "SELECT {} FROM {} {suffix}",
        columns.join(","),
        kind.table()
    ))?;
    let mut rows = statement.query(params_from_iter(params))?;
    let (mut nodes, mut edges) = (Vec::new(), Vec::new());
    while let Some(row) = rows.next()? {
        budget.enter(3)?;
        for _ in 0..columns.len() - 1 {
            budget.enter(4)?;
        }
        // props occupies one field but its root is counted by parse_properties.
        // Avoid double-counting its root to match the complete bounded reader.
        match kind {
            Kind::Node => {
                let id = bounded::field(row, 0)?;
                let label = bounded::field(row, 1)?;
                let props = bounded::parse_properties(&bounded::field(row, 2)?, budget)?;
                nodes.push(Node { id, label, props });
            }
            Kind::Edge => {
                let src = bounded::field(row, 0)?;
                let dst = bounded::field(row, 1)?;
                let label = bounded::field(row, 2)?;
                let props = bounded::parse_properties(&bounded::field(row, 3)?, budget)?;
                edges.push(Edge {
                    src,
                    dst,
                    label,
                    props,
                });
            }
        }
    }
    Ok((nodes, edges))
}
fn after_node(id: Option<&str>, rows: usize) -> (String, Vec<SqlValue>) {
    match id {
        Some(id) => (
            "WHERE id COLLATE BINARY > ?1 ORDER BY id COLLATE BINARY LIMIT ?2".into(),
            vec![id.to_owned().into(), (rows as i64).into()],
        ),
        None => (
            "ORDER BY id COLLATE BINARY LIMIT ?1".into(),
            vec![(rows as i64).into()],
        ),
    }
}
fn after_edge(key: Option<&EdgeKey>, rows: usize) -> (String, Vec<SqlValue>) {
    match key {
        Some(key) => (
            "WHERE (src,dst,label) > (?1 COLLATE BINARY,?2 COLLATE BINARY,?3 COLLATE BINARY) ORDER BY src COLLATE BINARY,dst COLLATE BINARY,label COLLATE BINARY LIMIT ?4".into(),
            vec![
                key.src.clone().into(),
                key.dst.clone().into(),
                key.label.clone().into(),
                (rows as i64).into(),
            ],
        ),
        None => (
            "ORDER BY src COLLATE BINARY,dst COLLATE BINARY,label COLLATE BINARY LIMIT ?1".into(),
            vec![(rows as i64).into()],
        ),
    }
}
fn more(
    connection: &Connection,
    kind: Kind,
    suffix: &str,
    params: &[SqlValue],
) -> Result<bool, GraphError> {
    Ok(connection.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {} {suffix})", kind.table()),
        params_from_iter(params),
        |r| r.get(0),
    )?)
}
impl SqliteGraphStore {
    /// Probe only edge existence at a frozen revision, without copying keys or
    /// properties. This admits no graph row and grants no further read budget.
    pub fn has_edges_at_revision(&self, expected: &StoreRevision) -> Result<bool, GraphError> {
        let transaction = self.conn.unchecked_transaction()?;
        check_revision(&transaction, Some(expected))?;
        preflight_schema(&transaction)?;
        let exists = more(&transaction, Kind::Edge, "LIMIT 1", &[])?;
        transaction.commit()?;
        Ok(exists)
    }

    /// Read a bounded node keyset window without counting/admitting the whole graph.
    pub fn read_node_window(
        &self,
        request: &NodeWindowRequest,
    ) -> Result<FactWindow<Node, String>, GraphError> {
        validate_request(
            request.limits,
            request.expected_revision.as_ref(),
            request.after_id.is_some(),
            &request.after_id.as_deref().into_iter().collect::<Vec<_>>(),
        )?;
        let transaction = self.conn.unchecked_transaction()?;
        let revision = check_revision(&transaction, request.expected_revision.as_ref())?;
        preflight_schema(&transaction)?;
        let (suffix, params) = after_node(request.after_id.as_deref(), request.limits.max_rows);
        let mut remaining_bytes = request.limits.max_bytes;
        let mut budget = budget(request.limits)?;
        let graph = read_rows(
            &transaction,
            Kind::Node,
            &suffix,
            &params,
            request.limits,
            &mut budget,
            &mut remaining_bytes,
        )?;
        let canonical_bytes = bounded::canonical_graph_bytes(&graph, request.limits.max_bytes)?;
        #[cfg(any(test, feature = "test-support"))]
        if let Some(after_nodes) = self.snapshot_after_nodes.borrow_mut().take() {
            after_nodes()?;
        }
        let last = graph.0.last().map(|node| node.id.clone());
        let has_more = if let Some(last) = last.as_deref() {
            let (suffix, params) = after_node(Some(last), 1);
            more(&transaction, Kind::Node, &suffix, &params)?
        } else {
            false
        };
        transaction.commit()?;
        Ok(FactWindow {
            revision,
            scanned_raw_rows: graph.0.len(),
            rows: graph.0,
            raw_bytes: request.limits.max_bytes - remaining_bytes,
            canonical_bytes,
            json_values: budget.consumed_values(),
            next_key: if has_more { last } else { None },
            remaining: if has_more {
                Remaining::More
            } else {
                Remaining::Exhausted
            },
        })
    }
    /// Read a bounded edge keyset window in (src,dst,label) order.
    pub fn read_edge_window(
        &self,
        request: &EdgeWindowRequest,
    ) -> Result<FactWindow<Edge, EdgeKey>, GraphError> {
        let parts = request
            .after_key
            .as_ref()
            .map(|k| vec![k.src.as_str(), k.dst.as_str(), k.label.as_str()])
            .unwrap_or_default();
        validate_request(
            request.limits,
            request.expected_revision.as_ref(),
            request.after_key.is_some(),
            &parts,
        )?;
        let transaction = self.conn.unchecked_transaction()?;
        let revision = check_revision(&transaction, request.expected_revision.as_ref())?;
        preflight_schema(&transaction)?;
        let (suffix, params) = after_edge(request.after_key.as_ref(), request.limits.max_rows);
        let mut remaining_bytes = request.limits.max_bytes;
        let mut budget = budget(request.limits)?;
        let graph = read_rows(
            &transaction,
            Kind::Edge,
            &suffix,
            &params,
            request.limits,
            &mut budget,
            &mut remaining_bytes,
        )?;
        let canonical_bytes = bounded::canonical_graph_bytes(&graph, request.limits.max_bytes)?;
        let last = graph.1.last().map(|edge| EdgeKey {
            src: edge.src.clone(),
            dst: edge.dst.clone(),
            label: edge.label.clone(),
        });
        let has_more = if let Some(last) = last.as_ref() {
            let (suffix, params) = after_edge(Some(last), 1);
            more(&transaction, Kind::Edge, &suffix, &params)?
        } else {
            false
        };
        transaction.commit()?;
        Ok(FactWindow {
            revision,
            scanned_raw_rows: graph.1.len(),
            rows: graph.1,
            raw_bytes: request.limits.max_bytes - remaining_bytes,
            canonical_bytes,
            json_values: budget.consumed_values(),
            next_key: if has_more { last } else { None },
            remaining: if has_more {
                Remaining::More
            } else {
                Remaining::Exhausted
            },
        })
    }
    /// Read exact bounded facts/digests and source states without full-graph loading.
    /// Binding metadata is separately bounded by the source-association schema.
    pub fn read_selected_facts_at_revision(
        &self,
        keys: &[FactKey],
        expected: &StoreRevision,
        limits: SnapshotReadLimits,
    ) -> Result<SelectedFacts, GraphError> {
        self.read_selected_facts_at_revision_as(
            keys,
            expected,
            limits,
            source::BindingRole::Primary,
        )
    }

    /// [`Self::read_selected_facts_at_revision`] with each selection's state
    /// taken from the bindings of `role` (ADR-0079).
    pub fn read_selected_facts_at_revision_as(
        &self,
        keys: &[FactKey],
        expected: &StoreRevision,
        limits: SnapshotReadLimits,
        role: source::BindingRole,
    ) -> Result<SelectedFacts, GraphError> {
        validate_request(limits, Some(expected), false, &[])?;
        if keys.len() > source::MAX_SOURCE_SELECTION_KEYS || keys.len() > limits.max_rows {
            return Err(SnapshotBoundsError::RowLimit.into());
        }
        let mut key_bytes = 0usize;
        for key in keys {
            key.validate()?;
            let bytes = match key {
                FactKey::Node { id } => id.len(),
                FactKey::Edge {
                    source,
                    label,
                    destination,
                } => source.len() + label.len() + destination.len(),
            };
            key_bytes = key_bytes
                .checked_add(bytes)
                .ok_or(SnapshotBoundsError::RawByteLimit)?;
        }
        if key_bytes > limits.max_bytes {
            return Err(SnapshotBoundsError::RawByteLimit.into());
        }
        let transaction = self.conn.unchecked_transaction()?;
        let revision = check_revision(&transaction, Some(expected))?;
        preflight_schema(&transaction)?;
        source::preflight_selection_schema(&transaction)?;
        source::check_schema(&transaction)?;
        crate::coverage::preflight_schema(&transaction)?;
        let complete_coverage = source::omission_coverage_complete(&transaction)?;
        let mut remaining_bytes = limits.max_bytes;
        let mut budget = budget(limits)?;
        let (mut nodes, mut edges, mut selections) = (Vec::new(), Vec::new(), Vec::new());
        for fact in keys.iter().collect::<BTreeSet<_>>() {
            let (kind, suffix, params) = match fact {
                FactKey::Node { id } => (
                    Kind::Node,
                    "WHERE id COLLATE BINARY=?1 LIMIT 1",
                    vec![id.clone().into()],
                ),
                FactKey::Edge {
                    source,
                    label,
                    destination,
                } => (
                    Kind::Edge,
                    "WHERE src COLLATE BINARY=?1 AND dst COLLATE BINARY=?2 AND label COLLATE BINARY=?3 LIMIT 1",
                    vec![
                        source.clone().into(),
                        destination.clone().into(),
                        label.clone().into(),
                    ],
                ),
            };
            let selected_graph = read_rows(
                &transaction,
                kind,
                suffix,
                &params,
                limits,
                &mut budget,
                &mut remaining_bytes,
            )?;
            // Digest helpers allocate canonical hash inputs. Admit this row's
            // canonical expansion before invoking them, not only at final output.
            bounded::canonical_graph_bytes(&selected_graph, limits.max_bytes)?;
            let (mut n, mut e) = selected_graph;
            let digest = match kind {
                Kind::Node => n.first().map(source::node_digest).transpose()?,
                Kind::Edge => e.first().map(source::edge_digest).transpose()?,
            };
            let binding = source::load_selection_binding(&transaction, role, fact)?;
            // Omission rows record primary receipts only; an observed
            // selection never reads them, and no observed omission is recorded.
            let omission = match role {
                source::BindingRole::Primary => source::selection_omission(
                    &transaction,
                    fact,
                    digest.as_deref(),
                    complete_coverage,
                )?,
                source::BindingRole::Observed => source::OmissionLookup::Unrecorded,
            };
            #[cfg(any(test, feature = "test-support"))]
            if let Some(after_lookup) = self.source_binding_after_lookup.borrow_mut().take() {
                after_lookup()?;
            }
            let state = match (digest, binding) {
                (None, SelectionBinding::Absent) => FactSourceState::Missing,
                (Some(fact_digest), SelectionBinding::Absent) => {
                    FactSourceState::Absent { fact_digest }
                }
                (Some(fact_digest), SelectionBinding::Present(binding))
                    if binding.emitted_fact_digest == fact_digest =>
                {
                    FactSourceState::Present {
                        fact_digest,
                        binding,
                    }
                }
                (fact_digest, _) => FactSourceState::Invalid { fact_digest },
            };
            nodes.append(&mut n);
            edges.append(&mut e);
            selections.push(FactSourceSelection {
                fact: fact.clone(),
                state,
                omission,
            });
        }
        edges.sort_by(|a, b| (&a.src, &a.dst, &a.label).cmp(&(&b.src, &b.dst, &b.label)));
        let graph = (nodes, edges);
        let canonical_bytes = bounded::canonical_graph_bytes(&graph, limits.max_bytes)?;
        transaction.commit()?;
        Ok(SelectedFacts {
            revision,
            nodes: graph.0,
            edges: graph.1,
            selections,
            raw_bytes: limits.max_bytes - remaining_bytes,
            canonical_bytes,
        })
    }
}
#[cfg(test)]
mod tests;

mod adjacency;
pub use adjacency::{AdjacencyPage, ScopedGraphReader, ScopedReadUsage};

mod neighborhood;
pub use neighborhood::NeighborhoodGraph;

mod selection;
pub use selection::{FactFilter, MAX_FILTER_LABELS};
