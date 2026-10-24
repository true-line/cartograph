//! Versioned logical payload units for bounded read acquisition. These are
//! named DTO/payload/index roles, not allocator layouts or a heap/RSS bound.
//! Native stacks, collection capacity slack and database caches are excluded.
use crate::coverage::{
    CoverageBasis, CoverageContext, CoverageQualification, RepoQualification, ScopedRepo,
};
use crate::{ContextFact, ContextSnapshot, FactReference};
use core_graph::coverage::{CoverageReadLimits, MAX_INVENTORY_BYTES, StoredCoverage};
use core_graph::{Edge, Node, SnapshotReadLimits};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    io::{self, Write},
    mem::size_of,
};

mod coverage_validation;
pub use coverage_validation::coverage_batch_validation_workspace;

mod query;
pub use query::{QueryWorkspace, query_workspace};

/// Sum named roles, failing on the first unrepresentable total.
pub fn sum(roles: &[usize]) -> Option<usize> {
    roles.iter().try_fold(0usize, |a, b| a.checked_add(*b))
}
/// Explicit logical vector/DTO slots, excluding allocator slack.
pub fn slots<T>(count: usize) -> Option<usize> {
    count.checked_mul(size_of::<T>())
}
/// Logical ordered-set links, not a standard-library B-tree layout claim.
pub fn set<K>(count: usize) -> Option<usize> {
    count.checked_mul(sum(&[size_of::<K>(), slots::<usize>(2)?])?)
}
/// Logical ordered-map links, with separately accounted owned key payload.
pub fn map<K, V>(count: usize) -> Option<usize> {
    count.checked_mul(sum(&[size_of::<K>(), size_of::<V>(), slots::<usize>(3)?])?)
}
/// Uniform JSON currency: every value gets a string and container-entry role,
/// even when its actual variant needs neither. Payload occurs exactly once.
pub fn json(bytes: usize, values: usize) -> Option<usize> {
    sum(&[
        bytes,
        slots::<Value>(values)?,
        slots::<String>(values)?,
        map::<(), ()>(values)?,
    ])
}
/// Byte-admitted typed JSON currency when no explicit value bound exists.
pub fn json_bytes(bytes: usize) -> Option<usize> {
    json(bytes, bytes.checked_add(1)?)
}
/// Maximum JSON string encoding of raw UTF-8 bytes. Six is the exact encoding
/// length of an ASCII control byte (\\u0000), not a safety coefficient.
pub fn string_wire(bytes: usize) -> Option<usize> {
    bytes
        .checked_mul("\\u0000".len())?
        .checked_add("\"\"".len())
}
/// Count compact serialization without constructing a full output buffer.
pub fn count(value: &impl Serialize) -> Option<usize> {
    encoded_size(value).ok()
}

/// Exact compact JSON length with serialization errors preserved, without an
/// output byte buffer. Query diagnostics need the full rejected length too.
pub(crate) fn encoded_size(value: &impl Serialize) -> Result<usize, serde_json::Error> {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| io::Error::other("logical size overflow"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Counter(0);
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.0)
}
/// Owned raw graph role; row slots use the larger of the two actual DTOs.
pub fn graph(limits: SnapshotReadLimits) -> Option<usize> {
    sum(&[
        limits.max_bytes,
        limits
            .max_rows
            .checked_mul(size_of::<Node>().max(size_of::<Edge>()))?,
        json(0, limits.max_values)?,
    ])
}
/// Snapshot/index and identity construction roles; no second string payload is
/// charged for canonicalization, which moves leaves while rebuilding containers.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotReservation {
    /// Full owned snapshot including facts and indexes.
    pub retained: usize,
    /// Additional complete identity Value, container rebuild and output bytes.
    pub identity_peak: usize,
    /// Session visibility identity set, separate from snapshot indexes.
    pub visibility: usize,
}
/// Bound the concrete ContextSnapshot construction used for raw and projected
/// identities. Its two directed neighbor entries have separate named roles.
pub fn snapshot(limits: SnapshotReadLimits) -> Option<SnapshotReservation> {
    let n = limits.max_rows;
    let b = limits.max_bytes;
    let v = limits.max_values;
    let node_ids = sum(&[b, set::<String>(n)?])?;
    let neighbor_keys = sum(&[b, map::<String, BTreeSet<String>>(n)?])?;
    let forward = sum(&[b, set::<String>(n)?])?;
    let reverse = sum(&[b, set::<String>(n)?])?;
    let retained = sum(&[
        size_of::<ContextSnapshot>(),
        graph(limits)?,
        node_ids,
        neighbor_keys,
        forward,
        reverse,
        slots::<ContextFact>(n)?,
        "context-v1:".len(),
        blake3::OUT_LEN.checked_mul(2)?,
    ])?;
    let prefix = count(&("context-v1", (Vec::<Node>::new(), Vec::<Edge>::new())))?;
    let identity_peak = sum(&[
        json(b, v)?,
        map::<String, Value>(v)?,
        slots::<Value>(v)?,
        b,
        prefix,
    ])?;
    let visibility = sum(&[b, set::<FactReference>(n)?])?;
    Some(SnapshotReservation {
        retained,
        identity_peak,
        visibility,
    })
}
/// Reference-only scope selection owners; excludes the caller-admitted snapshot
/// and consumed scope, collection capacity slack and native/allocator/RSS memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceWorkspace {
    /// Returned reference set, conservatively charging the graph byte bound.
    pub references_bytes: usize,
    /// Reference set plus visited/frontier/next-frontier identities.
    pub peak_bytes: usize,
}
/// Checked logical ownership for `ContextSnapshot::references_in_scope`.
/// The scope is separately admitted and moved; an unknown anchor is returned
/// without cloning. No fact payload or serialized response is constructed.
pub fn reference_selection_workspace(limits: SnapshotReadLimits) -> Option<ReferenceWorkspace> {
    let ids = sum(&[
        size_of::<BTreeSet<String>>(),
        limits.max_bytes,
        set::<String>(limits.max_rows)?,
    ])?;
    let references_bytes = sum(&[
        size_of::<BTreeSet<FactReference>>(),
        limits.max_bytes,
        set::<FactReference>(limits.max_rows)?,
    ])?;
    Some(ReferenceWorkspace {
        references_bytes,
        peak_bytes: sum(&[references_bytes, ids.checked_mul(3)?])?,
    })
}

/// Module-owned coverage qualification and construction roles.
#[derive(Debug, Clone, Copy)]
pub struct CoverageReservation {
    /// Compact qualification/basis identity bound, also covers public envelope bytes.
    pub wire_bytes: usize,
    /// Complete stored inventory copies, including row headers.
    pub stored: usize,
    /// One current inventory parse workspace.
    pub inventory_parse: usize,
    /// Qualification including retained inventory, basis and public rows.
    pub retained: usize,
    /// Additional map indexes, identity Value/container rebuild and wire buffer.
    pub qualification_peak: usize,
}
/// Scope from the app has no layer hints. Coverage can add every stored repo to
/// the scoped union; retained inventories and original stored rows coexist.
pub fn coverage(
    limits: CoverageReadLimits,
    scoped_repos: usize,
    scope_identity_bytes: usize,
) -> Option<CoverageReservation> {
    let k = limits.max_records;
    let c = limits.max_bytes;
    let rows = k.checked_add(scoped_repos)?;
    let stored = sum(&[c, slots::<StoredCoverage>(k)?])?;
    let inventory_parse = json_bytes(c.min(MAX_INVENTORY_BYTES))?;
    let empty = RepoQualification {
        repo: String::new(),
        status: crate::coverage::CoverageStatus::Recorded,
        commit: Some(String::new()),
        inventory_hash: Some(String::new()),
        limited: Some(false),
        counts: Some(Default::default()),
        layer_hints: Some(Vec::new()),
        compared: vec![
            crate::coverage::COMPARED_REGISTRATION.into(),
            crate::coverage::COMPARED_PLUGINS.into(),
            crate::coverage::COMPARED_LAYER_HINTS.into(),
        ],
    };
    // One copy of inventory-derived commit/hints, and separately escaped raw
    // SQL/scope identities used by qualification rows and its private basis.
    let row_wire = sum(&[
        c,
        string_wire(c)?,
        string_wire(scope_identity_bytes)?,
        rows.checked_mul(count(&empty)?)?,
    ])?;
    let basis_wire = sum(&[
        string_wire(c)?,
        k.checked_mul(count(&CoverageBasis {
            repo: String::new(),
            registered_source_id: Some(String::new()),
            ingest_id: String::new(),
            config_hash: String::new(),
            inventory_hash: String::new(),
        })?)?,
    ])?;
    let inventories = sum(&[c, c, map::<String, (String, String)>(k)?])?;
    let retained = sum(&[
        size_of::<CoverageContext>(),
        size_of::<CoverageQualification>(),
        json_bytes(row_wire)?,
        slots::<RepoQualification>(rows)?,
        json_bytes(basis_wire)?,
        slots::<CoverageBasis>(k)?,
        inventories,
    ])?;
    let wire = sum(&[
        row_wire,
        basis_wire,
        count(&(
            "coverage-context-v1",
            crate::coverage::COVERAGE_CONTRACT,
            false,
        ))?,
    ])?;
    let qualification_peak = sum(&[
        inventory_parse,
        map::<&str, &ScopedRepo>(scoped_repos)?,
        map::<&str, &StoredCoverage>(k)?,
        set::<&str>(rows)?,
        json_bytes(wire)?,
        map::<String, Value>(wire.checked_add(1)?)?,
        wire,
    ])?;
    Some(CoverageReservation {
        wire_bytes: wire,
        stored,
        inventory_parse,
        retained,
        qualification_peak,
    })
}

/// Additional discovery phase roles. Transport owns the returned outer Value;
/// this charges borrowed matching indexes, typed candidate response, a complete
/// coverage clone and the preflight envelope serialization (even when rejected).
/// Frame-sized label/filter/query-identity roles belong to the caller's separate
/// argument workspace because this graph/coverage API has no transport input cap.
pub fn discovery_workspace_bytes(
    graph: SnapshotReadLimits,
    coverage_limits: CoverageReadLimits,
    scoped_repos: usize,
    scope_identity_bytes: usize,
    response_bytes: usize,
) -> Option<usize> {
    let coverage = coverage(coverage_limits, scoped_repos, scope_identity_bytes)?;
    let inner_budget = sum(&[coverage.retained, coverage.wire_bytes])?;
    let matching = sum(&[
        crate::discovery::logical_workspace(graph, response_bytes)?,
        coverage.retained,
    ])?;
    Some(inner_budget.max(matching))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_currency_covers_concrete_graph_and_index_payload() {
        let node = Node {
            id: "a".into(),
            label: "Rule".into(),
            props: serde_json::json!({"text":"payload"}),
        };
        let b = count(&(vec![node.clone()], Vec::<Edge>::new())).unwrap();
        let limits = SnapshotReadLimits {
            max_bytes: b,
            max_rows: 1,
            max_depth: 8,
            max_values: 16,
        };
        let bound = snapshot(limits).unwrap();
        let concrete = ContextSnapshot::new(vec![node], vec![]).unwrap();
        assert!(bound.retained >= size_of::<ContextSnapshot>() + size_of::<ContextFact>() + b);
        assert!(
            bound.identity_peak
                >= count(&(
                    "context-v1",
                    serde_json::json!([[{"id":"a","label":"Rule","props":{"text":"payload"}}],[]])
                ))
                .unwrap()
        );
        assert!(!concrete.id().is_empty());
        let larger = snapshot(SnapshotReadLimits {
            max_bytes: b + 1,
            ..limits
        })
        .unwrap();
        assert!(larger.retained >= bound.retained && larger.identity_peak >= bound.identity_peak);
        assert!(
            snapshot(SnapshotReadLimits {
                max_bytes: usize::MAX,
                ..limits
            })
            .is_none()
        );
    }
    #[test]
    fn coverage_roles_are_monotone_and_checked() {
        let one = coverage(
            CoverageReadLimits {
                max_records: 1,
                max_bytes: 1024,
            },
            1,
            128,
        )
        .unwrap();
        let two = coverage(
            CoverageReadLimits {
                max_records: 2,
                max_bytes: 2048,
            },
            2,
            256,
        )
        .unwrap();
        assert!(two.retained >= one.retained && two.qualification_peak >= one.qualification_peak);
        assert!(
            coverage(
                CoverageReadLimits {
                    max_records: usize::MAX,
                    max_bytes: 1
                },
                1,
                1
            )
            .is_none()
        );
        assert!(json_bytes(usize::MAX).is_none());
    }
    #[test]
    fn concrete_qualification_and_discovery_page_fit_named_roles() {
        let scope = vec![ScopedRepo {
            repo_key: "fixture/repo".into(),
            registered_source_id: Some("source".into()),
            config_hash: "configuration".into(),
            layer_hints: None,
        }];
        let concrete = CoverageQualification::new(&scope, &[]).unwrap();
        let coverage_limits = CoverageReadLimits {
            max_records: 1,
            max_bytes: 1024,
        };
        let roles = coverage(coverage_limits, 1, 128).unwrap();
        assert!(roles.retained >= count(&concrete).unwrap());
        let limits = SnapshotReadLimits {
            max_bytes: 1024,
            max_rows: 4,
            max_values: 64,
            max_depth: 8,
        };
        let one = discovery_workspace_bytes(limits, coverage_limits, 1, 128, 512).unwrap();
        let two = discovery_workspace_bytes(limits, coverage_limits, 1, 128, 1024).unwrap();
        assert!(one >= roles.retained && two >= one);
    }
}
