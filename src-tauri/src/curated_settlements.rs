//! The in-memory cache of what an export's second compile settled
//! (`curated::DerivedSettlements`, #1834), split out of `curated` so the
//! app state that holds it compiles in app-store (#1924). The lookup and
//! the compile stay in `curated`.
use core_graph::StoreRevision;
use std::collections::BTreeSet;
use std::sync::Mutex;

/// What the compile of `DerivedDecisions::Compile` last found, kept in
/// memory so that a decision matching nothing does not cost every export a
/// second compile (#1834). Nothing is persisted.
///
/// The answer is a function of the rows that compile reads (nodes, edges and
/// coverage inventories, all read in one transaction with the store
/// revision) and of the hashes looked for. It is kept under exactly those:
/// the store revision, the graph content hash and the set of hashes. A read
/// with any of the three different compiles again and replaces the entry,
/// so no answer is used for another graph or another set of decisions. A
/// compile that fails is not kept.
#[derive(Debug, Default)]
pub struct DerivedSettlements(pub Mutex<Option<Settled>>);

#[derive(Debug)]
pub struct Settled {
    pub revision: StoreRevision,
    pub content_hash: String,
    /// The decided hashes no stored fact carried.
    pub wanted: BTreeSet<String>,
    /// Those of `wanted` a curatable derived statement carries.
    pub carried: BTreeSet<String>,
}
