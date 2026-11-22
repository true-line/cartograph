//! Current primary-source associations, separate from canonical graph facts.
//!
//! A binding identifies an immutable producer receipt stored elsewhere. It does
//! not establish input closure, receipt availability, source freshness or tier.

use crate::{
    Edge, GraphError, GraphPatch, Node, SnapshotBoundsError, SnapshotReadLimits, SqliteGraphStore,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const MAX_ID_BYTES: usize = 8192;
const MAX_LABEL_BYTES: usize = 256;
const MAX_REPO_BYTES: usize = 256;
const MAX_RECEIPT_BYTES: usize = 256;
const MAX_BINDINGS: usize = 200_000;

/// Maximum raw selection entries, before duplicate identities are removed.
pub const MAX_SOURCE_SELECTION_KEYS: usize = 65;

/// Maximum per-repository omission records persisted before degrading to an
/// incomplete index entry.
pub const MAX_OMISSIONS: usize = 262_144;

/// Bound that caused a recovered fact's primary receipt to be omitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptOmissionKind {
    /// Per-file or per-repository declaration receipt cap was reached.
    Declarations,
    /// Per-repository rule receipt cap was reached.
    Rules,
    /// Receipt store capacity cap was reached while persisting receipts.
    StoreCapacity,
}

impl ReceiptOmissionKind {
    /// Stable SQLite and wire token (`declarations`, `rules`, `store_capacity`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Declarations => "declarations",
            Self::Rules => "rules",
            Self::StoreCapacity => "store_capacity",
        }
    }

    fn from_db(value: &str) -> Result<Self, GraphError> {
        match value {
            "declarations" => Ok(Self::Declarations),
            "rules" => Ok(Self::Rules),
            "store_capacity" => Ok(Self::StoreCapacity),
            _ => Err(GraphError::SourceBinding("invalid omission kind")),
        }
    }
}

/// Positive per-fact record that a primary receipt was omitted because a bound
/// was reached.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceOmission {
    /// Complete typed fact identity.
    pub fact: FactKey,
    /// Complete fact digest the omitted receipt was emitted for. A record is
    /// kept only while it equals the published fact's digest, so an omitted
    /// occurrence never describes different current content.
    pub emitted_fact_digest: String,
    /// Bound that omitted the fact's primary receipt.
    pub kind: ReceiptOmissionKind,
}

/// Per-repository omission records published alongside a graph patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceOmissions<'a> {
    /// Omitted facts observed while extracting and persisting the repository.
    pub records: &'a [SourceOmission],
    /// Whether every omitted receipt produced a [`FactKey`].
    pub complete: bool,
}

impl<'a> SourceOmissions<'a> {
    /// No omission producer ran; leaves the repository unrecorded.
    pub const NONE: Self = Self {
        records: &[],
        complete: false,
    };
}

/// Outcome of checking whether a current graph fact's primary receipt was omitted.
///
/// [`Self::NotOmitted`] is not evidence that a receipt exists; only a
/// [`SourceBinding`] is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OmissionLookup {
    /// The current fact matches a recorded receipt omission for `repo_key`.
    Omitted {
        /// Registered repository key that recorded the omission.
        repo_key: String,
        /// Bound that caused the receipt to be omitted.
        kind: ReceiptOmissionKind,
    },
    /// Every covered repository has a complete omission index and none recorded
    /// an omission for this current fact. This is not evidence that a receipt
    /// exists; only a [`SourceBinding`] is.
    NotOmitted,
    /// The fact is absent, its omission record is stale, coverage is empty, or
    /// any covered repository has a missing or incomplete omission index.
    Unrecorded,
}

/// Unambiguous node or directed-edge identity, matching context FactReference.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactKey {
    /// Node identity; its label is part of the complete fact digest.
    Node {
        /// Stable node identifier.
        id: String,
    },
    /// Directed edge identity.
    Edge {
        /// Source node identifier.
        source: String,
        /// Exact directed relation label.
        label: String,
        /// Destination node identifier.
        destination: String,
    },
}

impl FactKey {
    /// Stable identity of a node.
    pub fn from_node(node: &Node) -> Self {
        Self::Node {
            id: node.id.clone(),
        }
    }

    /// Stable identity of an edge.
    pub fn from_edge(edge: &Edge) -> Self {
        Self::Edge {
            source: edge.src.clone(),
            label: edge.label.clone(),
            destination: edge.dst.clone(),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), GraphError> {
        match self {
            Self::Node { id } => bounded(id, MAX_ID_BYTES),
            Self::Edge {
                source,
                label,
                destination,
            } => {
                bounded(source, MAX_ID_BYTES)?;
                bounded(label, MAX_LABEL_BYTES)?;
                bounded(destination, MAX_ID_BYTES)
            }
        }
    }
}

/// Exact current receipt for one complete emitted fact. No source text is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceBinding {
    /// Complete typed fact identity.
    pub fact: FactKey,
    /// Registered repository ownership supplied by the host.
    pub repo_key: String,
    /// Immutable producer receipt identity, not a source-access capability.
    pub receipt_id: String,
    /// Versioned digest of the entire emitted node or edge.
    pub emitted_fact_digest: String,
}

/// Owned graph and selected association metadata from the same read revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSelectionSnapshot {
    /// Complete graph in ordinary snapshot order, not just the selected facts.
    pub graph: (Vec<Node>, Vec<Edge>),
    /// Unique selections sorted by typed fact identity.
    pub selections: Vec<FactSourceSelection>,
    /// Store revision observed in the same transaction as the graph rows,
    /// associations and omission records.
    pub revision: crate::StoreRevision,
}

/// The observed source association state for one requested typed identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactSourceSelection {
    /// Requested identity, including when its fact is missing.
    pub fact: FactKey,
    /// Association and complete-fact state in the copied graph revision.
    pub state: FactSourceState,
    /// Receipt omission outcome read in the same transaction as `state`,
    /// against the copied graph's fact digest (AC-0314). Only
    /// [`OmissionLookup::Omitted`] is evidence; [`OmissionLookup::NotOmitted`]
    /// never means a receipt exists.
    pub omission: OmissionLookup,
}

/// A source association never establishes receipt availability or source truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactSourceState {
    /// Neither the selected fact nor an association exists.
    Missing,
    /// The fact exists and has no current association.
    Absent {
        /// Digest of the complete selected fact.
        fact_digest: String,
    },
    /// Bounded, valid metadata matches the complete selected fact.
    Present {
        /// Digest of the complete selected fact.
        fact_digest: String,
        /// Exact current association, without a receipt body or source bytes.
        binding: SourceBinding,
    },
    /// An association is malformed, orphaned or does not match the fact.
    Invalid {
        /// Digest when the selected fact exists; absent for an orphan binding.
        fact_digest: Option<String>,
    },
}

impl SourceBinding {
    fn validate(&self) -> Result<(), GraphError> {
        self.fact.validate()?;
        bounded(&self.repo_key, MAX_REPO_BYTES)?;
        bounded(&self.receipt_id, MAX_RECEIPT_BYTES)?;
        let prefix = match self.fact {
            FactKey::Node { .. } => "node-v1:",
            FactKey::Edge { .. } => "edge-v1:",
        };
        let digest = self
            .emitted_fact_digest
            .strip_prefix(prefix)
            .ok_or(GraphError::SourceBinding("invalid complete fact digest"))?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(GraphError::SourceBinding("invalid complete fact digest"));
        }
        Ok(())
    }
}

/// Canonical digest of every node field, including all properties/provenance.
pub fn node_digest(node: &Node) -> Result<String, GraphError> {
    digest("node-v1", serde_json::to_value(node)?)
}

/// Canonical digest of every edge field, including all properties/provenance.
pub fn edge_digest(edge: &Edge) -> Result<String, GraphError> {
    digest("edge-v1", serde_json::to_value(edge)?)
}

fn digest(prefix: &str, value: Value) -> Result<String, GraphError> {
    let bytes = serde_json::to_vec(&(format!("cartograph.source.{prefix}"), canonicalize(value)))?;
    Ok(format!("{prefix}:{}", core_prov::content_hash(&bytes)))
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Object(properties) => {
            let sorted: BTreeMap<_, _> = properties
                .into_iter()
                .map(|(key, value)| (key, canonicalize(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        scalar => scalar,
    }
}

fn bounded(value: &str, maximum: usize) -> Result<(), GraphError> {
    if value.is_empty() || value.len() > maximum || value.contains('\0') {
        return Err(GraphError::SourceBinding("invalid bounded identity"));
    }
    Ok(())
}

// Exact owned SQL is checked before use. A partial/future schema is never
// silently CREATE-repaired, and this version does not use graph user_version.
const META_SCHEMA_V2: &str = "CREATE TABLE source_binding_meta (singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1), version INTEGER NOT NULL CHECK (version = 2)) STRICT";
const META_SCHEMA: &str = "CREATE TABLE source_binding_meta (singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1), version INTEGER NOT NULL CHECK (version = 3)) STRICT";
const SCHEMA_VERSION: i64 = 3;

const SCHEMA_V1: &[(&str, &str, &str)] = &[
    (
        "table",
        "source_binding_meta",
        "CREATE TABLE source_binding_meta (singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1), version INTEGER NOT NULL CHECK (version = 1)) STRICT",
    ),
    (
        "table",
        "source_node_bindings",
        "CREATE TABLE source_node_bindings (node_id TEXT PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE CHECK (length(CAST(node_id AS BLOB)) BETWEEN 1 AND 8192), repo_key TEXT NOT NULL CHECK (length(CAST(repo_key AS BLOB)) BETWEEN 1 AND 256), receipt_id TEXT NOT NULL CHECK (length(CAST(receipt_id AS BLOB)) BETWEEN 1 AND 256), emitted_fact_digest TEXT NOT NULL CHECK (length(CAST(emitted_fact_digest AS BLOB)) = 72)) STRICT",
    ),
    (
        "table",
        "source_edge_bindings",
        "CREATE TABLE source_edge_bindings (source TEXT NOT NULL CHECK (length(CAST(source AS BLOB)) BETWEEN 1 AND 8192), label TEXT NOT NULL CHECK (length(CAST(label AS BLOB)) BETWEEN 1 AND 256), destination TEXT NOT NULL CHECK (length(CAST(destination AS BLOB)) BETWEEN 1 AND 8192), repo_key TEXT NOT NULL CHECK (length(CAST(repo_key AS BLOB)) BETWEEN 1 AND 256), receipt_id TEXT NOT NULL CHECK (length(CAST(receipt_id AS BLOB)) BETWEEN 1 AND 256), emitted_fact_digest TEXT NOT NULL CHECK (length(CAST(emitted_fact_digest AS BLOB)) = 72), PRIMARY KEY (source, label, destination), FOREIGN KEY (source, destination, label) REFERENCES edges(src, dst, label) ON DELETE CASCADE) STRICT",
    ),
    (
        "index",
        "source_binding_nodes_repo",
        "CREATE INDEX source_binding_nodes_repo ON source_node_bindings(repo_key)",
    ),
    (
        "index",
        "source_binding_edges_repo",
        "CREATE INDEX source_binding_edges_repo ON source_edge_bindings(repo_key)",
    ),
    (
        "trigger",
        "source_binding_node_insert",
        "CREATE TRIGGER source_binding_node_insert AFTER INSERT ON nodes BEGIN DELETE FROM source_node_bindings WHERE node_id = NEW.id; DELETE FROM source_edge_bindings WHERE source = NEW.id OR destination = NEW.id; END",
    ),
    (
        "trigger",
        "source_binding_node_update",
        "CREATE TRIGGER source_binding_node_update BEFORE UPDATE ON nodes BEGIN DELETE FROM source_node_bindings WHERE node_id = OLD.id OR node_id = NEW.id; DELETE FROM source_edge_bindings WHERE OLD.id != NEW.id AND (source = OLD.id OR destination = OLD.id OR source = NEW.id OR destination = NEW.id); END",
    ),
    (
        "trigger",
        "source_binding_node_delete",
        "CREATE TRIGGER source_binding_node_delete BEFORE DELETE ON nodes BEGIN DELETE FROM source_node_bindings WHERE node_id = OLD.id; DELETE FROM source_edge_bindings WHERE source = OLD.id OR destination = OLD.id; END",
    ),
    (
        "trigger",
        "source_binding_edge_insert",
        "CREATE TRIGGER source_binding_edge_insert AFTER INSERT ON edges BEGIN DELETE FROM source_edge_bindings WHERE source = NEW.src AND label = NEW.label AND destination = NEW.dst; END",
    ),
    (
        "trigger",
        "source_binding_edge_update",
        "CREATE TRIGGER source_binding_edge_update BEFORE UPDATE ON edges BEGIN DELETE FROM source_edge_bindings WHERE (source = OLD.src AND label = OLD.label AND destination = OLD.dst) OR (source = NEW.src AND label = NEW.label AND destination = NEW.dst); END",
    ),
    (
        "trigger",
        "source_binding_edge_delete",
        "CREATE TRIGGER source_binding_edge_delete BEFORE DELETE ON edges BEGIN DELETE FROM source_edge_bindings WHERE source = OLD.src AND label = OLD.label AND destination = OLD.dst; END",
    ),
];

const OMISSIONS_SCHEMA: &[(&str, &str, &str)] = &[
    (
        "table",
        "source_node_omissions",
        "CREATE TABLE source_node_omissions (node_id TEXT PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE CHECK (length(CAST(node_id AS BLOB)) BETWEEN 1 AND 8192), repo_key TEXT NOT NULL CHECK (length(CAST(repo_key AS BLOB)) BETWEEN 1 AND 256), kind TEXT NOT NULL CHECK (kind IN ('declarations','rules','store_capacity')), emitted_fact_digest TEXT NOT NULL CHECK (length(CAST(emitted_fact_digest AS BLOB)) = 72)) STRICT",
    ),
    (
        "table",
        "source_edge_omissions",
        "CREATE TABLE source_edge_omissions (source TEXT NOT NULL CHECK (length(CAST(source AS BLOB)) BETWEEN 1 AND 8192), label TEXT NOT NULL CHECK (length(CAST(label AS BLOB)) BETWEEN 1 AND 256), destination TEXT NOT NULL CHECK (length(CAST(destination AS BLOB)) BETWEEN 1 AND 8192), repo_key TEXT NOT NULL CHECK (length(CAST(repo_key AS BLOB)) BETWEEN 1 AND 256), kind TEXT NOT NULL CHECK (kind IN ('declarations','rules','store_capacity')), emitted_fact_digest TEXT NOT NULL CHECK (length(CAST(emitted_fact_digest AS BLOB)) = 72), PRIMARY KEY (source, label, destination), FOREIGN KEY (source, destination, label) REFERENCES edges(src, dst, label) ON DELETE CASCADE) STRICT",
    ),
    (
        "table",
        "source_omission_index",
        "CREATE TABLE source_omission_index (repo_key TEXT PRIMARY KEY NOT NULL CHECK (length(CAST(repo_key AS BLOB)) BETWEEN 1 AND 256), state TEXT NOT NULL CHECK (state IN ('complete','incomplete')), recorded INTEGER NOT NULL CHECK (recorded >= 0)) STRICT",
    ),
    (
        "index",
        "source_omission_nodes_repo",
        "CREATE INDEX source_omission_nodes_repo ON source_node_omissions(repo_key)",
    ),
    (
        "index",
        "source_omission_edges_repo",
        "CREATE INDEX source_omission_edges_repo ON source_edge_omissions(repo_key)",
    ),
    (
        "trigger",
        "source_omission_node_insert",
        "CREATE TRIGGER source_omission_node_insert AFTER INSERT ON nodes BEGIN DELETE FROM source_node_omissions WHERE node_id = NEW.id; DELETE FROM source_edge_omissions WHERE source = NEW.id OR destination = NEW.id; END",
    ),
    (
        "trigger",
        "source_omission_node_update",
        "CREATE TRIGGER source_omission_node_update BEFORE UPDATE ON nodes BEGIN DELETE FROM source_node_omissions WHERE node_id = OLD.id OR node_id = NEW.id; DELETE FROM source_edge_omissions WHERE OLD.id != NEW.id AND (source = OLD.id OR destination = OLD.id OR source = NEW.id OR destination = NEW.id); END",
    ),
    (
        "trigger",
        "source_omission_node_delete",
        "CREATE TRIGGER source_omission_node_delete BEFORE DELETE ON nodes BEGIN DELETE FROM source_node_omissions WHERE node_id = OLD.id; DELETE FROM source_edge_omissions WHERE source = OLD.id OR destination = OLD.id; END",
    ),
    (
        "trigger",
        "source_omission_edge_insert",
        "CREATE TRIGGER source_omission_edge_insert AFTER INSERT ON edges BEGIN DELETE FROM source_edge_omissions WHERE source = NEW.src AND label = NEW.label AND destination = NEW.dst; END",
    ),
    (
        "trigger",
        "source_omission_edge_update",
        "CREATE TRIGGER source_omission_edge_update BEFORE UPDATE ON edges BEGIN DELETE FROM source_edge_omissions WHERE (source = OLD.src AND label = OLD.label AND destination = OLD.dst) OR (source = NEW.src AND label = NEW.label AND destination = NEW.dst); END",
    ),
    (
        "trigger",
        "source_omission_edge_delete",
        "CREATE TRIGGER source_omission_edge_delete BEFORE DELETE ON edges BEGIN DELETE FROM source_edge_omissions WHERE source = OLD.src AND label = OLD.label AND destination = OLD.dst; END",
    ),
];

const SCHEMA_V2: &[(&str, &str, &str)] = &[
    ("table", "source_binding_meta", META_SCHEMA_V2),
    (
        "table",
        "source_node_bindings",
        "CREATE TABLE source_node_bindings (node_id TEXT PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE CHECK (length(CAST(node_id AS BLOB)) BETWEEN 1 AND 8192), repo_key TEXT NOT NULL CHECK (length(CAST(repo_key AS BLOB)) BETWEEN 1 AND 256), receipt_id TEXT NOT NULL CHECK (length(CAST(receipt_id AS BLOB)) BETWEEN 1 AND 256), emitted_fact_digest TEXT NOT NULL CHECK (length(CAST(emitted_fact_digest AS BLOB)) = 72)) STRICT",
    ),
    (
        "table",
        "source_edge_bindings",
        "CREATE TABLE source_edge_bindings (source TEXT NOT NULL CHECK (length(CAST(source AS BLOB)) BETWEEN 1 AND 8192), label TEXT NOT NULL CHECK (length(CAST(label AS BLOB)) BETWEEN 1 AND 256), destination TEXT NOT NULL CHECK (length(CAST(destination AS BLOB)) BETWEEN 1 AND 8192), repo_key TEXT NOT NULL CHECK (length(CAST(repo_key AS BLOB)) BETWEEN 1 AND 256), receipt_id TEXT NOT NULL CHECK (length(CAST(receipt_id AS BLOB)) BETWEEN 1 AND 256), emitted_fact_digest TEXT NOT NULL CHECK (length(CAST(emitted_fact_digest AS BLOB)) = 72), PRIMARY KEY (source, label, destination), FOREIGN KEY (source, destination, label) REFERENCES edges(src, dst, label) ON DELETE CASCADE) STRICT",
    ),
    (
        "index",
        "source_binding_nodes_repo",
        "CREATE INDEX source_binding_nodes_repo ON source_node_bindings(repo_key)",
    ),
    (
        "index",
        "source_binding_edges_repo",
        "CREATE INDEX source_binding_edges_repo ON source_edge_bindings(repo_key)",
    ),
    (
        "trigger",
        "source_binding_node_insert",
        "CREATE TRIGGER source_binding_node_insert AFTER INSERT ON nodes BEGIN DELETE FROM source_node_bindings WHERE node_id = NEW.id; DELETE FROM source_edge_bindings WHERE source = NEW.id OR destination = NEW.id; END",
    ),
    (
        "trigger",
        "source_binding_node_update",
        "CREATE TRIGGER source_binding_node_update BEFORE UPDATE ON nodes BEGIN DELETE FROM source_node_bindings WHERE node_id = OLD.id OR node_id = NEW.id; DELETE FROM source_edge_bindings WHERE OLD.id != NEW.id AND (source = OLD.id OR destination = OLD.id OR source = NEW.id OR destination = NEW.id); END",
    ),
    (
        "trigger",
        "source_binding_node_delete",
        "CREATE TRIGGER source_binding_node_delete BEFORE DELETE ON nodes BEGIN DELETE FROM source_node_bindings WHERE node_id = OLD.id; DELETE FROM source_edge_bindings WHERE source = OLD.id OR destination = OLD.id; END",
    ),
    (
        "trigger",
        "source_binding_edge_insert",
        "CREATE TRIGGER source_binding_edge_insert AFTER INSERT ON edges BEGIN DELETE FROM source_edge_bindings WHERE source = NEW.src AND label = NEW.label AND destination = NEW.dst; END",
    ),
    (
        "trigger",
        "source_binding_edge_update",
        "CREATE TRIGGER source_binding_edge_update BEFORE UPDATE ON edges BEGIN DELETE FROM source_edge_bindings WHERE (source = OLD.src AND label = OLD.label AND destination = OLD.dst) OR (source = NEW.src AND label = NEW.label AND destination = NEW.dst); END",
    ),
    (
        "trigger",
        "source_binding_edge_delete",
        "CREATE TRIGGER source_binding_edge_delete BEFORE DELETE ON edges BEGIN DELETE FROM source_edge_bindings WHERE source = OLD.src AND label = OLD.label AND destination = OLD.dst; END",
    ),
    (
        "table",
        "source_node_omissions",
        "CREATE TABLE source_node_omissions (node_id TEXT PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE CHECK (length(CAST(node_id AS BLOB)) BETWEEN 1 AND 8192), repo_key TEXT NOT NULL CHECK (length(CAST(repo_key AS BLOB)) BETWEEN 1 AND 256), kind TEXT NOT NULL CHECK (kind IN ('declarations','rules','store_capacity')), emitted_fact_digest TEXT NOT NULL CHECK (length(CAST(emitted_fact_digest AS BLOB)) = 72)) STRICT",
    ),
    (
        "table",
        "source_edge_omissions",
        "CREATE TABLE source_edge_omissions (source TEXT NOT NULL CHECK (length(CAST(source AS BLOB)) BETWEEN 1 AND 8192), label TEXT NOT NULL CHECK (length(CAST(label AS BLOB)) BETWEEN 1 AND 256), destination TEXT NOT NULL CHECK (length(CAST(destination AS BLOB)) BETWEEN 1 AND 8192), repo_key TEXT NOT NULL CHECK (length(CAST(repo_key AS BLOB)) BETWEEN 1 AND 256), kind TEXT NOT NULL CHECK (kind IN ('declarations','rules','store_capacity')), emitted_fact_digest TEXT NOT NULL CHECK (length(CAST(emitted_fact_digest AS BLOB)) = 72), PRIMARY KEY (source, label, destination), FOREIGN KEY (source, destination, label) REFERENCES edges(src, dst, label) ON DELETE CASCADE) STRICT",
    ),
    (
        "table",
        "source_omission_index",
        "CREATE TABLE source_omission_index (repo_key TEXT PRIMARY KEY NOT NULL CHECK (length(CAST(repo_key AS BLOB)) BETWEEN 1 AND 256), state TEXT NOT NULL CHECK (state IN ('complete','incomplete')), recorded INTEGER NOT NULL CHECK (recorded >= 0)) STRICT",
    ),
    (
        "index",
        "source_omission_nodes_repo",
        "CREATE INDEX source_omission_nodes_repo ON source_node_omissions(repo_key)",
    ),
    (
        "index",
        "source_omission_edges_repo",
        "CREATE INDEX source_omission_edges_repo ON source_edge_omissions(repo_key)",
    ),
    (
        "trigger",
        "source_omission_node_insert",
        "CREATE TRIGGER source_omission_node_insert AFTER INSERT ON nodes BEGIN DELETE FROM source_node_omissions WHERE node_id = NEW.id; DELETE FROM source_edge_omissions WHERE source = NEW.id OR destination = NEW.id; END",
    ),
    (
        "trigger",
        "source_omission_node_update",
        "CREATE TRIGGER source_omission_node_update BEFORE UPDATE ON nodes BEGIN DELETE FROM source_node_omissions WHERE node_id = OLD.id OR node_id = NEW.id; DELETE FROM source_edge_omissions WHERE OLD.id != NEW.id AND (source = OLD.id OR destination = OLD.id OR source = NEW.id OR destination = NEW.id); END",
    ),
    (
        "trigger",
        "source_omission_node_delete",
        "CREATE TRIGGER source_omission_node_delete BEFORE DELETE ON nodes BEGIN DELETE FROM source_node_omissions WHERE node_id = OLD.id; DELETE FROM source_edge_omissions WHERE source = OLD.id OR destination = OLD.id; END",
    ),
    (
        "trigger",
        "source_omission_edge_insert",
        "CREATE TRIGGER source_omission_edge_insert AFTER INSERT ON edges BEGIN DELETE FROM source_edge_omissions WHERE source = NEW.src AND label = NEW.label AND destination = NEW.dst; END",
    ),
    (
        "trigger",
        "source_omission_edge_update",
        "CREATE TRIGGER source_omission_edge_update BEFORE UPDATE ON edges BEGIN DELETE FROM source_edge_omissions WHERE (source = OLD.src AND label = OLD.label AND destination = OLD.dst) OR (source = NEW.src AND label = NEW.label AND destination = NEW.dst); END",
    ),
    (
        "trigger",
        "source_omission_edge_delete",
        "CREATE TRIGGER source_omission_edge_delete BEFORE DELETE ON edges BEGIN DELETE FROM source_edge_omissions WHERE source = OLD.src AND label = OLD.label AND destination = OLD.dst; END",
    ),
];

/// The observed binding role (ADR-0079, AC-1047): the same shape as the
/// primary binding tables, invalidated by the same fact writes.
const OBSERVATIONS_SCHEMA: &[(&str, &str, &str)] = &[
    (
        "table",
        "source_node_observations",
        "CREATE TABLE source_node_observations (node_id TEXT PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE CHECK (length(CAST(node_id AS BLOB)) BETWEEN 1 AND 8192), repo_key TEXT NOT NULL CHECK (length(CAST(repo_key AS BLOB)) BETWEEN 1 AND 256), receipt_id TEXT NOT NULL CHECK (length(CAST(receipt_id AS BLOB)) BETWEEN 1 AND 256), emitted_fact_digest TEXT NOT NULL CHECK (length(CAST(emitted_fact_digest AS BLOB)) = 72)) STRICT",
    ),
    (
        "table",
        "source_edge_observations",
        "CREATE TABLE source_edge_observations (source TEXT NOT NULL CHECK (length(CAST(source AS BLOB)) BETWEEN 1 AND 8192), label TEXT NOT NULL CHECK (length(CAST(label AS BLOB)) BETWEEN 1 AND 256), destination TEXT NOT NULL CHECK (length(CAST(destination AS BLOB)) BETWEEN 1 AND 8192), repo_key TEXT NOT NULL CHECK (length(CAST(repo_key AS BLOB)) BETWEEN 1 AND 256), receipt_id TEXT NOT NULL CHECK (length(CAST(receipt_id AS BLOB)) BETWEEN 1 AND 256), emitted_fact_digest TEXT NOT NULL CHECK (length(CAST(emitted_fact_digest AS BLOB)) = 72), PRIMARY KEY (source, label, destination), FOREIGN KEY (source, destination, label) REFERENCES edges(src, dst, label) ON DELETE CASCADE) STRICT",
    ),
    (
        "index",
        "source_observation_nodes_repo",
        "CREATE INDEX source_observation_nodes_repo ON source_node_observations(repo_key)",
    ),
    (
        "index",
        "source_observation_edges_repo",
        "CREATE INDEX source_observation_edges_repo ON source_edge_observations(repo_key)",
    ),
    (
        "trigger",
        "source_observation_node_insert",
        "CREATE TRIGGER source_observation_node_insert AFTER INSERT ON nodes BEGIN DELETE FROM source_node_observations WHERE node_id = NEW.id; DELETE FROM source_edge_observations WHERE source = NEW.id OR destination = NEW.id; END",
    ),
    (
        "trigger",
        "source_observation_node_update",
        "CREATE TRIGGER source_observation_node_update BEFORE UPDATE ON nodes BEGIN DELETE FROM source_node_observations WHERE node_id = OLD.id OR node_id = NEW.id; DELETE FROM source_edge_observations WHERE OLD.id != NEW.id AND (source = OLD.id OR destination = OLD.id OR source = NEW.id OR destination = NEW.id); END",
    ),
    (
        "trigger",
        "source_observation_node_delete",
        "CREATE TRIGGER source_observation_node_delete BEFORE DELETE ON nodes BEGIN DELETE FROM source_node_observations WHERE node_id = OLD.id; DELETE FROM source_edge_observations WHERE source = OLD.id OR destination = OLD.id; END",
    ),
    (
        "trigger",
        "source_observation_edge_insert",
        "CREATE TRIGGER source_observation_edge_insert AFTER INSERT ON edges BEGIN DELETE FROM source_edge_observations WHERE source = NEW.src AND label = NEW.label AND destination = NEW.dst; END",
    ),
    (
        "trigger",
        "source_observation_edge_update",
        "CREATE TRIGGER source_observation_edge_update BEFORE UPDATE ON edges BEGIN DELETE FROM source_edge_observations WHERE (source = OLD.src AND label = OLD.label AND destination = OLD.dst) OR (source = NEW.src AND label = NEW.label AND destination = NEW.dst); END",
    ),
    (
        "trigger",
        "source_observation_edge_delete",
        "CREATE TRIGGER source_observation_edge_delete BEFORE DELETE ON edges BEGIN DELETE FROM source_edge_observations WHERE source = OLD.src AND label = OLD.label AND destination = OLD.dst; END",
    ),
];

/// The current schema: v2's objects under the v3 meta, plus the observed
/// binding role.
static SCHEMA: std::sync::LazyLock<Vec<(&str, &str, &str)>> = std::sync::LazyLock::new(|| {
    std::iter::once(("table", "source_binding_meta", META_SCHEMA))
        .chain(SCHEMA_V2.iter().skip(1).copied())
        .chain(OBSERVATIONS_SCHEMA.iter().copied())
        .collect()
});

const OWNED_SCHEMA_WHERE: &str = "name GLOB 'source_binding_*' OR name GLOB 'source_omission_*' OR name GLOB 'source_observation_*' OR name IN ('source_node_bindings', 'source_edge_bindings', 'source_node_omissions', 'source_edge_omissions', 'source_node_observations', 'source_edge_observations') OR (tbl_name IN ('source_binding_meta', 'source_node_bindings', 'source_edge_bindings', 'source_node_omissions', 'source_edge_omissions', 'source_omission_index', 'source_node_observations', 'source_edge_observations') AND name NOT GLOB 'sqlite_*') OR (type = 'trigger' AND tbl_name IN ('nodes', 'edges'))";

pub(crate) fn preflight_selection_schema(connection: &Connection) -> Result<(), GraphError> {
    // Only the opt-in bounded path changes admission behavior. Before the
    // existing exact-schema decoder copies SQL/name bodies, bound their count,
    // types and UTF-8 byte lengths in the same read transaction.
    let (count, valid): (i64, bool) = connection.query_row(
        &format!("SELECT count(*), coalesce(min(typeof(name) = 'text' AND length(CAST(name AS BLOB)) <= 256 AND typeof(type) = 'text' AND length(CAST(type AS BLOB)) <= 16 AND typeof(sql) = 'text' AND length(CAST(sql AS BLOB)) <= 4096), 0) FROM sqlite_schema WHERE {OWNED_SCHEMA_WHERE}"),
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let generation_present = crate::generation::validate_optional(connection)?;
    // These are the watched tables selected by OWNED_SCHEMA_WHERE. Derive the
    // trigger count from the generation registry rather than duplicating it.
    let expected = SCHEMA.len()
        + if generation_present {
            crate::generation::trigger_count_for_tables(&[
                "nodes",
                "edges",
                "source_node_bindings",
                "source_edge_bindings",
            ])
        } else {
            0
        };
    if usize::try_from(count).ok() != Some(expected) || !valid {
        return Err(SnapshotBoundsError::InvalidSchema.into());
    }
    Ok(())
}

fn owned_schema(connection: &Connection) -> Result<BTreeMap<String, (String, String)>, GraphError> {
    // Include arbitrarily named triggers/unique indexes affecting these tables,
    // not just objects with our prefix: they can suppress or rewrite publication.
    let mut statement = connection.prepare(&format!(
        "SELECT name, type, sql FROM sqlite_schema WHERE {OWNED_SCHEMA_WHERE}"
    ))?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn matches_schema(
    actual: &BTreeMap<String, (String, String)>,
    expected: &[(&str, &str, &str)],
) -> bool {
    actual.len() == expected.len()
        && expected.iter().all(|(kind, name, sql)| {
            actual
                .get(*name)
                .map(|(actual_kind, actual_sql)| (actual_kind.as_str(), actual_sql.as_str()))
                == Some((*kind, *sql))
        })
}

fn has_version(connection: &Connection, version: i64) -> Result<bool, GraphError> {
    let mut statement =
        connection.prepare("SELECT singleton, version FROM source_binding_meta LIMIT 2")?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows == [(1, version)])
}

pub(crate) fn check_schema(connection: &Connection) -> Result<(), GraphError> {
    // Generation triggers are part of this owner's inventory. Validate their
    // metadata bounds before owned_schema allocates SQL strings.
    let generation = crate::generation::validate_for_owner(connection)?;
    let mut actual = owned_schema(connection)?;
    generation.strip_objects(&mut actual);
    if !matches_schema(&actual, &SCHEMA) {
        return Err(GraphError::SourceBinding("incompatible association schema"));
    }
    if !has_version(connection, SCHEMA_VERSION)? {
        return Err(GraphError::SourceBinding("unsupported association version"));
    }
    Ok(())
}

pub(crate) fn initialize(transaction: &Transaction<'_>) -> Result<(), GraphError> {
    let connection: &Connection = transaction;
    let generation = crate::generation::validate_for_owner(connection)?;
    let mut actual = owned_schema(connection)?;
    generation.strip_objects(&mut actual);
    // Each exact earlier version migrates additively in this transaction:
    // v1 gains the omission tables (AC-0314), v1 and v2 the observed binding
    // tables (AC-1047), all empty.
    let added: &[&[(&str, &str, &str)]] = if actual.is_empty() {
        &[SCHEMA_V2.split_at(1).1, OBSERVATIONS_SCHEMA]
    } else if matches_schema(&actual, SCHEMA_V1) && has_version(connection, 1)? {
        &[OMISSIONS_SCHEMA, OBSERVATIONS_SCHEMA]
    } else if matches_schema(&actual, SCHEMA_V2) && has_version(connection, 2)? {
        &[OBSERVATIONS_SCHEMA]
    } else {
        &[]
    };
    if !added.is_empty() {
        for (_, _, sql) in added.iter().flat_map(|objects| objects.iter()) {
            transaction.execute_batch(sql)?;
        }
        if !actual.is_empty() {
            transaction.execute_batch("DROP TABLE source_binding_meta")?;
        }
        transaction.execute_batch(META_SCHEMA)?;
        transaction.execute(
            "INSERT INTO source_binding_meta(singleton, version) VALUES (1, ?1)",
            [SCHEMA_VERSION],
        )?;
    }
    check_schema(connection)?;
    Ok(())
}

pub(crate) fn clear(transaction: &Transaction<'_>) -> Result<(), GraphError> {
    check_schema(transaction)?;
    // The omission index is not generation-watched; an actual deletion is a
    // row event even when no fact or coverage row is removed with it.
    if transaction.execute("DELETE FROM source_omission_index", [])? > 0 {
        crate::generation::bump(transaction)?;
    }
    Ok(())
}

/// Which association a fact's source binding plays (ADR-0079): the
/// primary declaration binding, or the observed binding to a captured
/// observation input. Each has its own pair of tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingRole {
    /// The fact's declaration, bound to its registered source's receipt.
    Primary,
    /// The fact's observation, bound to its source's input receipt.
    Observed,
}

impl BindingRole {
    fn tables(self) -> (&'static str, &'static str) {
        match self {
            Self::Primary => ("source_node_bindings", "source_edge_bindings"),
            Self::Observed => ("source_node_observations", "source_edge_observations"),
        }
    }
}

fn load_binding(
    connection: &Connection,
    fact: &FactKey,
) -> Result<Option<SourceBinding>, GraphError> {
    load_role(connection, BindingRole::Primary, fact)
}

fn load_role(
    connection: &Connection,
    role: BindingRole,
    fact: &FactKey,
) -> Result<Option<SourceBinding>, GraphError> {
    let (nodes, edges) = role.tables();
    let row: Option<(String, String, String)> = match fact {
        FactKey::Node { id } => connection.prepare_cached(&format!(
            "SELECT repo_key, receipt_id, emitted_fact_digest FROM {nodes} WHERE node_id = ?1",
        ))?.query_row([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()?,
        FactKey::Edge { source, label, destination } => connection.prepare_cached(&format!(
            "SELECT repo_key, receipt_id, emitted_fact_digest FROM {edges} WHERE source = ?1 AND label = ?2 AND destination = ?3",
        ))?.query_row(params![source, label, destination], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()?,
    };
    row.map(|(repo_key, receipt_id, emitted_fact_digest)| {
        let binding = SourceBinding {
            fact: fact.clone(),
            repo_key,
            receipt_id,
            emitted_fact_digest,
        };
        binding.validate()?;
        Ok(binding)
    })
    .transpose()
}

/// Omission row for `fact`. Cell types and byte lengths are checked in SQL
/// before any body is copied; a malformed row is `Some(None)`, which callers
/// treat as unrecorded rather than as positive evidence.
#[allow(clippy::type_complexity)]
fn load_omission(
    connection: &Connection,
    fact: &FactKey,
) -> Result<Option<Option<(String, ReceiptOmissionKind, String)>>, GraphError> {
    let table_and_key = match fact {
        FactKey::Node { .. } => "source_node_omissions WHERE node_id = ?1",
        FactKey::Edge { .. } => {
            "source_edge_omissions WHERE source = ?1 AND label = ?2 AND destination = ?3"
        }
    };
    let sql = format!(
        "SELECT
         CASE WHEN typeof(repo_key) = 'text' AND length(CAST(repo_key AS BLOB)) BETWEEN 1 AND {MAX_REPO_BYTES} THEN CAST(repo_key AS BLOB) END,
         CASE WHEN typeof(kind) = 'text' AND length(CAST(kind AS BLOB)) BETWEEN 1 AND 16 THEN CAST(kind AS BLOB) END,
         CASE WHEN typeof(emitted_fact_digest) = 'text' AND length(CAST(emitted_fact_digest AS BLOB)) = 72 THEN CAST(emitted_fact_digest AS BLOB) END
         FROM {table_and_key}"
    );
    let read = |row: &rusqlite::Row<'_>| {
        Ok((
            row.get::<_, Option<Vec<u8>>>(0)?,
            row.get::<_, Option<Vec<u8>>>(1)?,
            row.get::<_, Option<Vec<u8>>>(2)?,
        ))
    };
    let row = match fact {
        FactKey::Node { id } => connection.query_row(&sql, [id], read).optional()?,
        FactKey::Edge {
            source,
            label,
            destination,
        } => connection
            .query_row(&sql, params![source, label, destination], read)
            .optional()?,
    };
    let Some((repo_key, kind, emitted_fact_digest)) = row else {
        return Ok(None);
    };
    let text = |bytes: Option<Vec<u8>>| bytes.and_then(|bytes| String::from_utf8(bytes).ok());
    let (Some(repo_key), Some(kind), Some(emitted_fact_digest)) =
        (text(repo_key), text(kind), text(emitted_fact_digest))
    else {
        return Ok(Some(None));
    };
    let Ok(kind) = ReceiptOmissionKind::from_db(&kind) else {
        return Ok(Some(None));
    };
    Ok(Some(Some((repo_key, kind, emitted_fact_digest))))
}

/// Whether every coverage-recorded repo has a complete omission index, and at
/// least one exists. Read inside the caller's selection transaction.
pub(crate) fn omission_coverage_complete(connection: &Connection) -> Result<bool, GraphError> {
    crate::coverage::check_schema(connection)?;
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM coverage_inventories)
            AND NOT EXISTS(
                SELECT 1 FROM coverage_inventories c
                WHERE NOT EXISTS(
                    SELECT 1 FROM source_omission_index i
                    WHERE i.repo_key = c.repo_key AND i.state = 'complete'
                )
            )",
        [],
        |row| row.get(0),
    )?)
}

/// Omission outcome for `fact` given its digest in the copied (bounded) graph
/// revision, so no unbounded fact read happens here.
pub(crate) fn selection_omission(
    connection: &Connection,
    fact: &FactKey,
    fact_digest: Option<&str>,
    complete_coverage: bool,
) -> Result<OmissionLookup, GraphError> {
    let Some(fact_digest) = fact_digest else {
        return Ok(OmissionLookup::Unrecorded);
    };
    // Binding precedence: any current association row, from any repository,
    // means a primary receipt is held for this fact, so an omission row left
    // by an earlier publication (possibly another repository's) never reads
    // as Omitted beside it.
    let omission = match load_selection_binding(connection, BindingRole::Primary, fact)? {
        SelectionBinding::Absent => load_omission(connection, fact)?,
        SelectionBinding::Invalid | SelectionBinding::Present(_) => None,
    };
    Ok(match omission {
        Some(Some((repo_key, kind, emitted_fact_digest))) if emitted_fact_digest == fact_digest => {
            OmissionLookup::Omitted { repo_key, kind }
        }
        Some(_) => OmissionLookup::Unrecorded,
        None if complete_coverage => OmissionLookup::NotOmitted,
        None => OmissionLookup::Unrecorded,
    })
}

fn replace_omissions(
    transaction: &Transaction<'_>,
    repo_key: &str,
    omissions: SourceOmissions<'_>,
) -> Result<(), GraphError> {
    let mut removed = 0;
    for table in [
        "source_node_omissions",
        "source_edge_omissions",
        "source_omission_index",
    ] {
        removed += transaction.execute(
            &format!("DELETE FROM {table} WHERE repo_key = ?1"),
            [repo_key],
        )?;
    }
    if omissions == SourceOmissions::NONE {
        // Omission tables are not generation-watched, so their row events
        // advance the store revision here, once per publication.
        if removed > 0 {
            crate::generation::bump(transaction)?;
        }
        return Ok(());
    }
    // Every other path rewrites the index row, a row event that must advance
    // the revision even when graph and binding rows are unchanged.
    crate::generation::bump(transaction)?;
    if omissions.records.len() > MAX_OMISSIONS {
        let recorded = i64::try_from(omissions.records.len())
            .map_err(|_| GraphError::SourceBinding("too many omissions"))?;
        transaction.execute(
            "INSERT INTO source_omission_index(repo_key, state, recorded) VALUES (?1, 'incomplete', ?2)",
            params![repo_key, recorded],
        )?;
        return Ok(());
    }
    let mut by_fact: BTreeMap<&FactKey, Vec<&SourceOmission>> = BTreeMap::new();
    for omission in omissions.records {
        omission.fact.validate()?;
        if omission.emitted_fact_digest.len() != 72 {
            return Err(GraphError::SourceBinding("invalid omission digest"));
        }
        by_fact.entry(&omission.fact).or_default().push(omission);
    }
    let mut inserted: i64 = 0;
    for (fact, records) in by_fact {
        let Some(emitted_fact_digest) = current_digest(transaction, fact)? else {
            continue;
        };
        // A current binding (from any repository, including this publication)
        // means a primary receipt was retained, so the fact is not omitted.
        if load_binding(transaction, fact)?.is_some() {
            continue;
        }
        // Only occurrences emitted for the published content describe it;
        // among those, keep the smallest kind.
        let Some(kind) = records
            .iter()
            .filter(|record| record.emitted_fact_digest == emitted_fact_digest)
            .map(|record| record.kind)
            .min()
        else {
            continue;
        };
        let changes = match fact {
            FactKey::Node { id } => transaction.prepare_cached(
                "INSERT OR IGNORE INTO source_node_omissions(node_id, repo_key, kind, emitted_fact_digest) VALUES (?1, ?2, ?3, ?4)",
            )?.execute(params![id, repo_key, kind.as_str(), emitted_fact_digest])?,
            FactKey::Edge {
                source,
                label,
                destination,
            } => transaction.prepare_cached(
                "INSERT OR IGNORE INTO source_edge_omissions(source, label, destination, repo_key, kind, emitted_fact_digest) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?.execute(params![
                source,
                label,
                destination,
                repo_key,
                kind.as_str(),
                emitted_fact_digest
            ])?,
        };
        inserted += changes as i64;
    }
    let state = if omissions.complete {
        "complete"
    } else {
        "incomplete"
    };
    transaction.execute(
        "INSERT INTO source_omission_index(repo_key, state, recorded) VALUES (?1, ?2, ?3)",
        params![repo_key, state, inserted],
    )?;
    Ok(())
}

pub(crate) enum SelectionBinding {
    Absent,
    Invalid,
    Present(SourceBinding),
}

pub(crate) fn load_selection_binding(
    connection: &Connection,
    role: BindingRole,
    fact: &FactKey,
) -> Result<SelectionBinding, GraphError> {
    let (nodes, edges) = role.tables();
    let table_and_key = match fact {
        FactKey::Node { .. } => format!("{nodes} WHERE node_id = ?1"),
        FactKey::Edge { .. } => {
            format!("{edges} WHERE source = ?1 AND label = ?2 AND destination = ?3")
        }
    };
    // Guard the serialized byte lengths in SQL before returning any body. BLOB
    // casts let malformed UTF-8 remain a per-key invalid association rather than
    // a String conversion error that aborts the complete selection window.
    let sql = format!(
        "SELECT
         CASE WHEN typeof(repo_key) = 'text' AND length(CAST(repo_key AS BLOB)) BETWEEN 1 AND {MAX_REPO_BYTES} THEN CAST(repo_key AS BLOB) END,
         CASE WHEN typeof(receipt_id) = 'text' AND length(CAST(receipt_id AS BLOB)) BETWEEN 1 AND {MAX_RECEIPT_BYTES} THEN CAST(receipt_id AS BLOB) END,
         CASE WHEN typeof(emitted_fact_digest) = 'text' AND length(CAST(emitted_fact_digest AS BLOB)) = 72 THEN CAST(emitted_fact_digest AS BLOB) END
         FROM {table_and_key}"
    );
    let read = |row: &rusqlite::Row<'_>| {
        Ok((
            row.get::<_, Option<Vec<u8>>>(0)?,
            row.get::<_, Option<Vec<u8>>>(1)?,
            row.get::<_, Option<Vec<u8>>>(2)?,
        ))
    };
    let row = match fact {
        FactKey::Node { id } => connection.query_row(&sql, [id], read).optional()?,
        FactKey::Edge {
            source,
            label,
            destination,
        } => connection
            .query_row(&sql, params![source, label, destination], read)
            .optional()?,
    };
    let Some((repo_key, receipt_id, emitted_fact_digest)) = row else {
        return Ok(SelectionBinding::Absent);
    };
    let (Some(repo_key), Some(receipt_id), Some(emitted_fact_digest)) = (
        repo_key.and_then(|bytes| String::from_utf8(bytes).ok()),
        receipt_id.and_then(|bytes| String::from_utf8(bytes).ok()),
        emitted_fact_digest.and_then(|bytes| String::from_utf8(bytes).ok()),
    ) else {
        return Ok(SelectionBinding::Invalid);
    };
    let binding = SourceBinding {
        fact: fact.clone(),
        repo_key,
        receipt_id,
        emitted_fact_digest,
    };
    Ok(if binding.validate().is_ok() {
        SelectionBinding::Present(binding)
    } else {
        SelectionBinding::Invalid
    })
}

fn snapshot_digest(
    graph: &(Vec<Node>, Vec<Edge>),
    fact: &FactKey,
) -> Result<Option<String>, GraphError> {
    // Ordinary snapshot rows already have these orders. Avoid reloading JSON or
    // allocating a second index over the entire graph for at most 65 identities.
    match fact {
        FactKey::Node { id } => graph
            .0
            .binary_search_by(|node| node.id.cmp(id))
            .ok()
            .map(|index| node_digest(&graph.0[index]))
            .transpose(),
        FactKey::Edge {
            source,
            label,
            destination,
        } => graph
            .1
            .binary_search_by(|edge| {
                (&edge.src, &edge.dst, &edge.label).cmp(&(source, destination, label))
            })
            .ok()
            .map(|index| edge_digest(&graph.1[index]))
            .transpose(),
    }
}

fn current_digest(connection: &Connection, fact: &FactKey) -> Result<Option<String>, GraphError> {
    match fact {
        FactKey::Node { id } => {
            let row: Option<(String, String)> = connection
                .prepare_cached("SELECT label, props FROM nodes WHERE id = ?1")?
                .query_row([id], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?;
            row.map(|(label, props)| {
                node_digest(&Node {
                    id: id.clone(),
                    label,
                    props: serde_json::from_str(&props)?,
                })
            })
            .transpose()
        }
        FactKey::Edge {
            source,
            label,
            destination,
        } => {
            let props: Option<String> = connection
                .prepare_cached(
                    "SELECT props FROM edges WHERE src = ?1 AND label = ?2 AND dst = ?3",
                )?
                .query_row(params![source, label, destination], |row| row.get(0))
                .optional()?;
            props
                .map(|props| {
                    edge_digest(&Edge {
                        src: source.clone(),
                        label: label.clone(),
                        dst: destination.clone(),
                        props: serde_json::from_str(&props)?,
                    })
                })
                .transpose()
        }
    }
}

impl SqliteGraphStore {
    /// Copy the complete graph and bounded, sorted unique source selections in
    /// one read transaction. More than 65 raw keys and invalid identities fail
    /// before database access. Empty selections still copy the complete graph.
    ///
    /// Schema, query and graph-data failures abort the operation; malformed
    /// selected association metadata instead yields a per-key `Invalid` state.
    /// The owned result grants no source access and establishes no input closure.
    pub fn read_source_selection_snapshot(
        &self,
        keys: &[FactKey],
    ) -> Result<SourceSelectionSnapshot, GraphError> {
        self.read_source_selection_snapshot_inner(keys, None, None)
    }

    /// Copy a complete bounded graph and the same source-selection states as
    /// [`Self::read_source_selection_snapshot`] in one transaction. The graph
    /// limit applies to every call, including incremental union rechecks after
    /// a live graph has grown. Limits and the existing raw 65-key cap are checked
    /// before database access; invalid selected bindings remain per-key states.
    pub fn read_source_selection_snapshot_bounded(
        &self,
        keys: &[FactKey],
        limits: SnapshotReadLimits,
    ) -> Result<SourceSelectionSnapshot, GraphError> {
        limits.validate()?;
        self.read_source_selection_snapshot_inner(keys, Some(limits), None)
    }

    /// Bounded selection that refuses with [`crate::GenerationError::Changed`]
    /// before reading graph rows when the store is no longer at `expected`, so
    /// a consumer can bind associations and omissions to its acquisition basis.
    pub fn read_source_selection_snapshot_at_revision_bounded(
        &self,
        expected: &crate::StoreRevision,
        keys: &[FactKey],
        limits: SnapshotReadLimits,
    ) -> Result<SourceSelectionSnapshot, GraphError> {
        limits.validate()?;
        self.read_source_selection_snapshot_inner(keys, Some(limits), Some(expected))
    }

    fn read_source_selection_snapshot_inner(
        &self,
        keys: &[FactKey],
        limits: Option<SnapshotReadLimits>,
        expected: Option<&crate::StoreRevision>,
    ) -> Result<SourceSelectionSnapshot, GraphError> {
        if keys.len() > MAX_SOURCE_SELECTION_KEYS {
            return Err(GraphError::SourceBinding("too many source selection keys"));
        }
        for key in keys {
            key.validate()?;
        }
        let unique: BTreeSet<_> = keys.iter().collect();
        let transaction = self.conn.unchecked_transaction()?;
        if limits.is_some() {
            preflight_selection_schema(&self.conn)?;
            crate::coverage::preflight_schema(&self.conn)?;
        }
        check_schema(&self.conn)?;
        let revision = crate::generation::read_store_revision(&transaction)?;
        if expected.is_some_and(|expected| expected != &revision) {
            return Err(crate::GenerationError::Changed.into());
        }
        let graph = match limits {
            Some(limits) => self.read_snapshot_rows_bounded(limits)?,
            None => self.read_snapshot_rows(None, None)?,
        };
        let selections = self.selections_in(&graph, unique)?;
        transaction.commit()?;
        Ok(SourceSelectionSnapshot {
            graph,
            selections,
            revision,
        })
    }

    /// The source-selection states of `keys` over a snapshot this store
    /// returned earlier, without copying the graph again: what
    /// [`Self::read_source_selection_snapshot`] would return as `selections`
    /// for `keys` while the store is still at `observed.revision`. It refuses
    /// with [`crate::GenerationError::Changed`] before reading any selection
    /// when the store has moved, so the states always describe the rows of
    /// `observed.graph`. The key cap and identity checks are the same and
    /// run before database access.
    pub fn read_source_selections_at(
        &self,
        observed: &SourceSelectionSnapshot,
        keys: &[FactKey],
    ) -> Result<Vec<FactSourceSelection>, GraphError> {
        if keys.len() > MAX_SOURCE_SELECTION_KEYS {
            return Err(GraphError::SourceBinding("too many source selection keys"));
        }
        for key in keys {
            key.validate()?;
        }
        let unique: BTreeSet<_> = keys.iter().collect();
        let transaction = self.conn.unchecked_transaction()?;
        check_schema(&self.conn)?;
        if crate::generation::read_store_revision(&transaction)? != observed.revision {
            return Err(crate::GenerationError::Changed.into());
        }
        let selections = self.selections_in(&observed.graph, unique)?;
        transaction.commit()?;
        Ok(selections)
    }

    /// The selection state of each key over `graph`, inside the caller's
    /// read transaction, which read `graph` or checked its revision.
    fn selections_in(
        &self,
        graph: &(Vec<Node>, Vec<Edge>),
        unique: BTreeSet<&FactKey>,
    ) -> Result<Vec<FactSourceSelection>, GraphError> {
        let complete_coverage = omission_coverage_complete(&self.conn)?;
        let mut selections = Vec::with_capacity(unique.len());
        for fact in unique {
            let fact_digest = snapshot_digest(graph, fact)?;
            let omission =
                selection_omission(&self.conn, fact, fact_digest.as_deref(), complete_coverage)?;
            let binding = load_selection_binding(&self.conn, BindingRole::Primary, fact)?;
            #[cfg(any(test, feature = "test-support"))]
            {
                let after_lookup = self.source_binding_after_lookup.borrow_mut().take();
                if let Some(after_lookup) = after_lookup {
                    after_lookup()?;
                }
            }
            let state = match (fact_digest, binding) {
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
            selections.push(FactSourceSelection {
                fact: fact.clone(),
                state,
                omission,
            });
        }
        Ok(selections)
    }

    /// Publish scoped facts and replace only this repo's current source bindings
    /// in one write transaction. Every requested binding must match its final
    /// fact; missing, duplicate, wrong-owner or mismatched bindings roll back.
    pub fn apply_patch_with_source_bindings_if_snapshot_matches(
        &mut self,
        expected: &impl crate::ExpectedSnapshot,
        patch: &GraphPatch,
        repo_key: &str,
        bindings: &[SourceBinding],
    ) -> Result<bool, GraphError> {
        self.apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
            expected,
            patch,
            repo_key,
            bindings,
            None,
            SourceOmissions::NONE,
        )
    }

    /// [`Self::apply_patch_with_source_bindings_if_snapshot_matches`], also
    /// replacing this repo's recorded coverage inventory and receipt omission
    /// records in the same write transaction (AC-0239, AC-0314). `None` removes
    /// the old inventory, which no longer describes the published facts, and
    /// [`SourceOmissions::NONE`] leaves the repository omission state unrecorded.
    pub fn apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
        &mut self,
        expected: &impl crate::ExpectedSnapshot,
        patch: &GraphPatch,
        repo_key: &str,
        bindings: &[SourceBinding],
        coverage: Option<&crate::coverage::CoverageRecord>,
        omissions: SourceOmissions<'_>,
    ) -> Result<bool, GraphError> {
        self.apply_patch_with_source_roles_and_coverage_if_snapshot_matches(
            expected,
            patch,
            repo_key,
            bindings,
            &[],
            coverage,
            omissions,
        )
    }

    /// [`Self::apply_patch_with_source_bindings_and_coverage_if_snapshot_matches`],
    /// also replacing this repo's observed bindings with `observations` in the
    /// same write transaction (ADR-0079, AC-1047). Each role is checked like
    /// the primary one: every association must match its final fact, and no
    /// fact may hold another repo's association of the same role.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_patch_with_source_roles_and_coverage_if_snapshot_matches(
        &mut self,
        expected: &impl crate::ExpectedSnapshot,
        patch: &GraphPatch,
        repo_key: &str,
        bindings: &[SourceBinding],
        observations: &[SourceBinding],
        coverage: Option<&crate::coverage::CoverageRecord>,
        omissions: SourceOmissions<'_>,
    ) -> Result<bool, GraphError> {
        bounded(repo_key, MAX_REPO_BYTES)?;
        let roles = [
            (BindingRole::Primary, bindings),
            (BindingRole::Observed, observations),
        ];
        for (_, associations) in roles {
            if associations.len() > MAX_BINDINGS {
                return Err(GraphError::SourceBinding("too many associations"));
            }
            let mut seen = BTreeSet::new();
            for binding in associations {
                binding.validate()?;
                if binding.repo_key != repo_key || !seen.insert(&binding.fact) {
                    return Err(GraphError::SourceBinding(
                        "duplicate or wrong-owner association",
                    ));
                }
            }
        }
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        check_schema(&self.conn)?;
        if !self.snapshot_matches(expected)? {
            return Ok(false);
        }
        // Check before patch triggers invalidate touched rows: a caller cannot
        // replace another repo's association by upserting its fact first.
        for (role, associations) in roles {
            for binding in associations {
                if load_role(&self.conn, role, &binding.fact)?
                    .is_some_and(|old| old.repo_key != repo_key)
                {
                    return Err(GraphError::SourceBinding(
                        "fact belongs to another source association",
                    ));
                }
            }
        }
        // Observed rows are not generation-watched; a change to them alone is
        // still a row event (ADR-0079).
        let mut observed_changed = 0;
        for (role, _) in roles {
            let (nodes, edges) = role.tables();
            let removed = transaction.execute(
                &format!("DELETE FROM {nodes} WHERE repo_key = ?1"),
                [repo_key],
            )? + transaction.execute(
                &format!("DELETE FROM {edges} WHERE repo_key = ?1"),
                [repo_key],
            )?;
            if role == BindingRole::Observed {
                observed_changed += removed;
            }
        }
        self.apply_patch_rows(patch)?;
        for (role, associations) in roles {
            let (nodes, edges) = role.tables();
            for binding in associations {
                if current_digest(&self.conn, &binding.fact)?.as_deref()
                    != Some(binding.emitted_fact_digest.as_str())
                {
                    return Err(GraphError::SourceBinding(
                        "missing fact or complete fact digest mismatch",
                    ));
                }
                let inserted = match &binding.fact {
                    FactKey::Node { id } => transaction.prepare_cached(&format!(
                        "INSERT INTO {nodes}(node_id, repo_key, receipt_id, emitted_fact_digest) VALUES (?1, ?2, ?3, ?4)",
                    ))?.execute(params![id, repo_key, binding.receipt_id, binding.emitted_fact_digest])?,
                    FactKey::Edge { source, label, destination } => transaction.prepare_cached(&format!(
                        "INSERT INTO {edges}(source, label, destination, repo_key, receipt_id, emitted_fact_digest) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    ))?.execute(params![source, label, destination, repo_key, binding.receipt_id, binding.emitted_fact_digest])?,
                };
                if inserted != 1 {
                    return Err(GraphError::SourceBinding("association publication failed"));
                }
                if role == BindingRole::Observed {
                    observed_changed += 1;
                }
            }
        }
        if observed_changed > 0 {
            crate::generation::bump(&transaction)?;
        }
        replace_omissions(&transaction, repo_key, omissions)?;
        crate::coverage::replace(&transaction, repo_key, coverage)?;
        transaction.commit()?;
        Ok(true)
    }

    /// Copy a binding and verify its complete fact in the same read revision.
    /// The caller must still validate receipt identity, registration and retained
    /// source in their own stores; this method grants no source access.
    pub fn current_source_binding(
        &self,
        fact: &FactKey,
    ) -> Result<Option<SourceBinding>, GraphError> {
        fact.validate()?;
        let transaction = self.conn.unchecked_transaction()?;
        check_schema(&self.conn)?;
        let binding = load_binding(&self.conn, fact)?;
        #[cfg(any(test, feature = "test-support"))]
        {
            let after_lookup = self.source_binding_after_lookup.borrow_mut().take();
            if let Some(after_lookup) = after_lookup {
                after_lookup()?;
            }
        }
        if let Some(binding) = &binding
            && current_digest(&self.conn, fact)?.as_deref()
                != Some(binding.emitted_fact_digest.as_str())
        {
            return Err(GraphError::SourceBinding(
                "missing fact or complete fact digest mismatch",
            ));
        }
        transaction.commit()?;
        Ok(binding)
    }

    /// Ordered current-association metadata for retention preview, without
    /// loading graph properties, receipt bodies or source bytes.
    pub fn source_bindings_for_repo(
        &self,
        repo_key: &str,
    ) -> Result<Vec<SourceBinding>, GraphError> {
        self.source_role_for_repo(BindingRole::Primary, repo_key)
    }

    /// [`Self::source_bindings_for_repo`] for the observed role (ADR-0079).
    pub fn source_observations_for_repo(
        &self,
        repo_key: &str,
    ) -> Result<Vec<SourceBinding>, GraphError> {
        self.source_role_for_repo(BindingRole::Observed, repo_key)
    }

    /// Copy a fact's observed binding and verify its complete fact in the
    /// same read revision, like [`Self::current_source_binding`] (ADR-0079).
    pub fn current_source_observation(
        &self,
        fact: &FactKey,
    ) -> Result<Option<SourceBinding>, GraphError> {
        fact.validate()?;
        let transaction = self.conn.unchecked_transaction()?;
        check_schema(&self.conn)?;
        let binding = load_role(&self.conn, BindingRole::Observed, fact)?;
        if let Some(binding) = &binding
            && current_digest(&self.conn, fact)?.as_deref()
                != Some(binding.emitted_fact_digest.as_str())
        {
            return Err(GraphError::SourceBinding(
                "missing fact or complete fact digest mismatch",
            ));
        }
        transaction.commit()?;
        Ok(binding)
    }

    fn source_role_for_repo(
        &self,
        role: BindingRole,
        repo_key: &str,
    ) -> Result<Vec<SourceBinding>, GraphError> {
        bounded(repo_key, MAX_REPO_BYTES)?;
        let (node_table, edge_table) = role.tables();
        let transaction = self.conn.unchecked_transaction()?;
        check_schema(&self.conn)?;
        let count: i64 = self.conn.query_row(
            &format!("SELECT (SELECT count(*) FROM {node_table} WHERE repo_key = ?1) + (SELECT count(*) FROM {edge_table} WHERE repo_key = ?1)"),
            [repo_key], |row| row.get(0),
        )?;
        if usize::try_from(count)
            .ok()
            .filter(|count| *count <= MAX_BINDINGS)
            .is_none()
        {
            return Err(GraphError::SourceBinding("too many associations"));
        }
        let mut bindings = Vec::new();
        {
            let mut statement = self.conn.prepare(&format!("SELECT node_id, receipt_id, emitted_fact_digest FROM {node_table} WHERE repo_key = ?1 ORDER BY node_id"))?;
            let rows = statement.query_map([repo_key], |row| {
                Ok(SourceBinding {
                    fact: FactKey::Node { id: row.get(0)? },
                    repo_key: repo_key.into(),
                    receipt_id: row.get(1)?,
                    emitted_fact_digest: row.get(2)?,
                })
            })?;
            for row in rows {
                let binding = row?;
                binding.validate()?;
                bindings.push(binding);
            }
        }
        {
            let mut statement = self.conn.prepare(&format!("SELECT source, label, destination, receipt_id, emitted_fact_digest FROM {edge_table} WHERE repo_key = ?1 ORDER BY source, label, destination"))?;
            let rows = statement.query_map([repo_key], |row| {
                Ok(SourceBinding {
                    fact: FactKey::Edge {
                        source: row.get(0)?,
                        label: row.get(1)?,
                        destination: row.get(2)?,
                    },
                    repo_key: repo_key.into(),
                    receipt_id: row.get(3)?,
                    emitted_fact_digest: row.get(4)?,
                })
            })?;
            for row in rows {
                let binding = row?;
                binding.validate()?;
                bindings.push(binding);
            }
        }
        transaction.commit()?;
        Ok(bindings)
    }

    /// One-shot deterministic cross-connection hook after association lookup,
    /// while the current-binding or selection-snapshot transaction is still open.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn set_source_binding_after_lookup_hook(
        &self,
        hook: impl FnOnce() -> Result<(), GraphError> + Send + 'static,
    ) {
        *self.source_binding_after_lookup.borrow_mut() = Some(Box::new(hook));
    }
}

#[cfg(test)]
mod tests;
