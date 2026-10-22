//! Named logical owner roles for partition construction; excludes caller's
//! frozen session, qualifiers, transport and core metadata workspace. No RSS claim.
use super::*;
use crate::workbench::accounting::{json, json_bytes, map_slots, set_slots, slots, sum};
/// Partition owner envelope, using the same units as selected compilation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartitionWorkspaceBound {
    /// Closure maps plus one current indexed page/raw row and typed selector.
    pub acquisition_bytes: usize,
    /// Nested typed rule-vector slots admitted by JSON values, before visit admission.
    pub typed_rule_vector_bytes: usize,
    /// Pending/done sets, map keys and entry/endpoint keys.
    pub frontier_bytes: usize,
    /// Reference vectors and visibility/index sets; no cloned projected graph.
    pub projection_bytes: usize,
    /// Existing module-owned native renderer maximum (data/rule subset).
    pub renderer_bytes: usize,
    /// Header, body, final content, assertions and fixed partition metadata.
    pub artifact_bytes: usize,
    /// Conservative sum of these separately named owners.
    pub peak_bytes: usize,
}
// Every vector element consumes a distinct JSON value. All variants live
// inline in these element DTOs (including SourceExpression/DependencyResolution
// enums); their nested vectors get separate entries here. String leaf payload
// moves out of the consumed Value and remains charged by B, not by this header.
fn typed_rule_vector_headers(values: usize) -> Option<usize> {
    use core_graph::rules::*;
    use std::mem::size_of;
    let header = [
        size_of::<BranchCondition>(),
        size_of::<RuleDependency>(),
        size_of::<SourceRedaction>(),
        size_of::<String>(),          // interpretation gap IDs
        size_of::<RedactionReason>(), // withheld literal reasons
        size_of::<LocalDefinition>(),
        size_of::<core_prov::EvidenceRef>(), // local-definition uses
        size_of::<DefinitionDependency>(),
        size_of::<DefinitionExpressionNode>(),
    ]
    .into_iter()
    .max()?;
    values.checked_mul(header)
}

/// Existing complete renderer accounting supplies a conservative data/rule
/// renderer envelope; this function adds partition acquisition and ownership
/// roles that the complete compiler never needs. No graph/trace work executes.
pub fn logical_partition_bound(
    graph: flowtracer::TraceGraphBound,
    trace: flowtracer::TraceLimits,
    limits: SelectedCompileLimits,
) -> Option<PartitionWorkspaceBound> {
    let b = graph.bytes;
    let n = graph.rows;
    let v = graph.values;
    let graph_owner = sum(&[
        json(b, v)?,
        n.checked_mul(std::mem::size_of::<Node>().max(std::mem::size_of::<Edge>()))?,
    ])?;
    let retained_maps = sum(&[
        graph_owner,
        map_slots::<String, Node>(n)?,
        map_slots::<(String, String, String), Edge>(n)?,
        b,
        b,
    ])?;
    // Retained closure, current page, current decode input and one typed
    // rule-reference parse are distinct owners even when they share a cap.
    let typed_rule_vector_bytes = typed_rule_vector_headers(v)?;
    let acquisition_bytes = sum(&[
        retained_maps,
        graph_owner,
        b,
        json(b, v)?,
        slots::<core_graph::rules::GuardedExitEvidence>(n)?,
        // Remaining cloned Value containers and growing typed vectors can
        // coexist during consuming deserialization; charge both role sets.
        typed_rule_vector_bytes,
        // The typed payload can name more graph references than the eventual
        // visit cap. This set exists BEFORE reader.node admission: use JSON
        // value count and payload bytes, never the closure row count.
        b,
        set_slots::<String>(v)?,
    ])?;
    let frontier_bytes = sum(&[
        b,
        set_slots::<String>(n)?, // pending entity frontier
        b,
        set_slots::<String>(n)?, // completed entity frontier
        b,
        slots::<String>(n)?, // sorted visible roots
        b,
        set_slots::<String>(n)?, // explicit inventory root set
        b,
        slots::<String>(3)?, // current entry/endpoint key clones
    ])?;
    // project_recovered_facts: selected node/edge Vecs, all-rule and visible-ID
    // sets; producer-local indexes are covered in renderer_bytes below.
    let projection_bytes = sum(&[
        slots::<&Node>(n)?,
        slots::<&Edge>(n)?,
        set_slots::<&str>(n)?,
        set_slots::<&str>(n)?,
        set_slots::<&str>(n)?,             // projected entity IDs
        set_slots::<&str>(n)?,             // projected available endpoint IDs
        map_slots::<&str, Vec<&str>>(n)?,  // entity adjacency
        slots::<&str>(n.checked_add(n)?)?, // both adjacency directions
        set_slots::<&str>(n)?,             // projected component
        set_slots::<&str>(n)?,             // projected frontier
        set_slots::<&str>(n)?,             // retained incident context IDs
    ])?;
    let renderer_bytes =
        crate::logical_compile_bound(graph, trace, limits)?.renderer_workspace_bytes;
    let w = limits.max_bytes;
    let fixed = sum(&[
        "spec-partition-basis-v1:".len(),
        64,
        "spec-partition-artifact-v1:".len(),
        64,
        "cartograph.spec-partition/rule-evidence-index@1".len(),
    ])?;
    let artifact_bytes = sum(&[
        w, // bounded partition header or JSON qualifier output
        w, // native body remains while concatenated into qualified content
        w, // qualified content
        json_bytes(w)?,
        slots::<SpecAssertion>(w.checked_add(1)?)?, // admitted assertions
        fixed,
        slots::<String>(4)?,
        std::mem::size_of::<PartitionArtifact>(),
    ])?;
    Some(PartitionWorkspaceBound {
        acquisition_bytes,
        typed_rule_vector_bytes,
        frontier_bytes,
        projection_bytes,
        renderer_bytes,
        artifact_bytes,
        peak_bytes: sum(&[
            acquisition_bytes,
            frontier_bytes,
            projection_bytes,
            renderer_bytes,
            artifact_bytes,
        ])?,
    })
}
