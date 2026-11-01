//! Capability-only additions to the existing selected compiler workspace.
use super::*;
use crate::workbench::capabilities::{Stories, Story, Support};
use core_graph::overlay::BaseRef;

/// Named logical owners, excluding retained base/selection and the caller's
/// `Vec<CapabilityStory>`; no allocator-capacity or RSS claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityCompileWorkspaceBound {
    /// Citation-entry maximum derived from selected JSON bytes.
    pub max_citations: usize,
    /// Borrowed base lookup/visibility and ordered candidate indexes.
    pub lookup_bytes: usize,
    /// Included Story headers and all retained/current support-vector slots.
    pub story_support_bytes: usize,
    /// Indexes and stories that remain live during selected compilation.
    pub validation_retained_bytes: usize,
    /// One current base or selected typed provenance decode.
    pub validation_scratch_bytes: usize,
    /// Validation-only peak, before the shared selected compiler.
    pub validation_peak_bytes: usize,
    /// Derived render vector and current selected-property/format/hash scratch.
    pub renderer_extra_bytes: usize,
    /// Complete peak; includes existing output accounting exactly once.
    pub peak_bytes: usize,
}

/// Compose the existing compiler bound with the Capability entrypoint's real
/// private indexes and support types. `selected_json_bytes` bounds complete
/// serialized OverlayFacts, not JSON value count or decoded container bytes.
/// All candidates/citations are admitted here, including filtered stories.
/// The host separately admits its borrowed candidate vector and retained inputs.
/// `trace` covers caller flows retained and projected during shared compilation;
/// legacy empty-flow callers may conservatively supply the same bound.
/// The security=None route produces the same bounded-size security rows: the
/// existing security_projection_bound/current-row roles still cover them.
pub fn logical_capability_compile_bound(
    graph: TraceGraphBound,
    trace: TraceLimits,
    compile: SelectedCompileLimits,
    selected_facts: usize,
    selected_json_bytes: usize,
) -> Option<CapabilityCompileWorkspaceBound> {
    let ordinary = logical_compile_bound(graph, trace, compile)?;
    let minimum_cite = count(&BaseRef::Node(String::new()))?.min(count(&BaseRef::Edge(
        String::new(),
        String::new(),
        String::new(),
    ))?);
    let max_citations = selected_json_bytes.checked_div(minimum_cite)?;
    let lookup_bytes = sum(&[
        map_slots::<&str, Option<&Node>>(graph.rows)?,
        map_slots::<(&String, &String, &String), Option<&Edge>>(graph.rows)?,
        slots::<&Node>(graph.rows)?,
        slots::<&Edge>(graph.rows)?,
        set_slots::<&str>(graph.rows)?,
        set_slots::<(&String, &String, &String)>(graph.rows)?,
        map_slots::<&str, &CapabilityStory<'_>>(selected_facts)?,
        slots::<BTreeMap<(), ()>>(3)?,
        slots::<BTreeSet<()>>(2)?,
        slots::<Vec<()>>(2)?,
    ])?;
    let story_support_bytes = sum(&[
        slots::<Story<'_>>(selected_facts)?,
        slots::<(&BaseRef, Support<'_>)>(max_citations)?,
        size_of::<Stories<'_>>(),
    ])?;
    let validation_retained_bytes = sum(&[lookup_bytes, story_support_bytes])?;
    let base_provenance = sum(&[
        json(graph.bytes, graph.values)?,
        slots::<EvidenceRef>(graph.values)?,
        size_of::<Provenance>(),
    ])?;
    let selected_provenance = selected_provenance_bytes(selected_json_bytes)?;
    let validation_scratch_bytes = base_provenance.max(selected_provenance);
    let validation_peak_bytes = sum(&[validation_retained_bytes, validation_scratch_bytes])?;
    let renderer = capability_renderer_parts(selected_facts, selected_json_bytes)?;
    let renderer_extra_bytes = sum(&[renderer.references_bytes, renderer.current_bytes])?;
    let peak_bytes = validation_peak_bytes.max(sum(&[
        ordinary.peak_bytes,
        validation_retained_bytes,
        renderer_extra_bytes,
    ])?);
    Some(CapabilityCompileWorkspaceBound {
        max_citations,
        lookup_bytes,
        story_support_bytes,
        validation_retained_bytes,
        validation_scratch_bytes,
        validation_peak_bytes,
        renderer_extra_bytes,
        peak_bytes,
    })
}

#[cfg(test)]
mod tests;

fn selected_provenance_bytes(bytes: usize) -> Option<usize> {
    sum(&[
        json_bytes(bytes)?,
        slots::<EvidenceRef>(bytes.checked_add(1)?)?,
        size_of::<Provenance>(),
    ])
}

pub(crate) fn capability_renderer_parts(
    count: usize,
    bytes: usize,
) -> Option<RendererWorkspaceParts> {
    Some(RendererWorkspaceParts {
        references_bytes: slots::<&Node>(count)?,
        current_bytes: sum(&[
            json_bytes(bytes)?,                // current provenance Value clone
            selected_provenance_bytes(bytes)?, // simultaneous typed decode
            bytes,                             // current node ID/name formatting
            crate::workbench::capabilities::support_id("", &BaseRef::Node(String::new())).len(),
            size_of::<blake3::Hasher>(),
        ])?,
    })
}
