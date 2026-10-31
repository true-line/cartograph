//! Logical temporary owners of [`GuardedExitEvidence::from_value`], not
//! allocator layouts or an RSS bound. Collection capacity slack is excluded.
use super::definitions::{
    DefinitionDependency, DefinitionExpressionNode, LocalDefinition, MAX_DEFINITION_CHAIN_DEPTH,
    MAX_DEFINITION_NODES, MAX_INITIALIZER_DEPENDENCIES, MAX_LOCAL_DEFINITIONS, MAX_USES,
};
use super::{
    BranchCondition, GuardedExitEvidence, RedactionReason, RuleDependency, SourceRedaction,
};
use core_prov::EvidenceRef;
use serde_json::Value;
use std::mem::size_of;

/// Nested internally or adjacently tagged enums on one decode path:
/// `LocalExit`, then `LiteralEvidence`, then `KnownLiteral`. Serde buffers
/// each tagged subtree into its own content tree before choosing a variant.
const TAGGED_DEPTH: usize = 3;
/// `collection_bounds` rejects more than 128 interpretation gap ids.
const MAX_GAP_IDS: usize = 128;

fn sum(roles: &[usize]) -> Option<usize> {
    roles.iter().try_fold(0usize, |a, b| a.checked_add(*b))
}
fn slots<T>(count: usize) -> Option<usize> {
    count.checked_mul(size_of::<T>())
}
fn set<K>(count: usize) -> Option<usize> {
    count.checked_mul(sum(&[size_of::<K>(), slots::<usize>(2)?])?)
}
fn map<K, V>(count: usize) -> Option<usize> {
    count.checked_mul(sum(&[size_of::<K>(), size_of::<V>(), slots::<usize>(3)?])?)
}
/// One value, string and map-entry role per JSON value, without payload.
fn containers(values: usize) -> Option<usize> {
    sum(&[
        slots::<Value>(values)?,
        slots::<String>(values)?,
        map::<(), ()>(values)?,
    ])
}

/// Heap owned by one decoded rule DTO beyond its inline shell: string
/// payloads come from distinct JSON string tokens (at most the wire), and each
/// collection slot is one admitted array item. Enums and nested structs are
/// inline in their parent slot. `rule_values` follows [`validation_workspace`].
pub fn decoded_heap_bytes(rule_wire: usize, rule_values: usize) -> Option<usize> {
    let values = rule_values.min(rule_wire.checked_add(1)?);
    // Typed elements are sized by the input, before any collection or
    // semantic refusal.
    let element = [
        size_of::<BranchCondition>(),
        size_of::<RuleDependency>(),
        size_of::<SourceRedaction>(),
        size_of::<RedactionReason>(),
        size_of::<String>(),
        size_of::<LocalDefinition>(),
        size_of::<EvidenceRef>(),
        size_of::<DefinitionDependency>(),
        size_of::<DefinitionExpressionNode>(),
    ]
    .into_iter()
    .max()?;
    rule_wire.checked_add(values.checked_mul(element)?)
}

/// Peak logical owners while `from_value` decodes and validates a rule whose
/// compact JSON is at most `rule_wire` bytes. The consumed input `Value` is
/// the caller's; the decoded DTO is included. Overflow returns `None`.
pub fn validation_workspace_bytes(rule_wire: usize) -> Option<usize> {
    // Every JSON value, and so every typed element, spends at least one byte.
    validation_workspace(rule_wire, rule_wire.checked_add(1)?)
}

/// [`validation_workspace_bytes`] with a separately admitted bound on the
/// rule's JSON values. `rule_values` must count every value the input holds,
/// including envelopes and malformed or unknown members, but not object keys.
/// Every typed element and every tagged subtree is at least one value. A
/// bound above the wire's own is never looser than the wire-only model.
pub fn validation_workspace(rule_wire: usize, rule_values: usize) -> Option<usize> {
    let values = rule_values.min(rule_wire.checked_add(1)?);
    // V2 preflight holds unvisited values with their depth.
    let preflight = slots::<(&Value, usize)>(values)?;
    let dto = sum(&[
        size_of::<GuardedExitEvidence>(),
        decoded_heap_bytes(rule_wire, values)?,
    ])?;
    let decode = sum(&[dto, containers(values)?.checked_mul(TAGGED_DEPTH)?])?;
    // V2 validation runs only after `collection_bounds`, so its scratch is
    // bounded by the same constants. Gaps, binding indices and declaration
    // spans stay alive through the arena and link checks.
    let retained = sum(&[
        set::<&str>(MAX_GAP_IDS)?,
        map::<&str, usize>(MAX_LOCAL_DEFINITIONS)?,
        set::<(u64, u64)>(MAX_LOCAL_DEFINITIONS)?,
    ])?;
    // One arena at a time. Each admitted node pushes its exit marker and at
    // most two children, and each pop may insert into the three index sets.
    let visits = MAX_DEFINITION_NODES.checked_mul(3)?.checked_add(1)?;
    let arena = sum(&[
        set::<usize>(visits)?.checked_mul(3)?,
        slots::<(usize, usize, bool)>(visits)?,
        slots::<u32>(2)?,
    ])?;
    // The chain walk keeps one sibling list per depth, plus the seeds. Paths
    // are cloned only after the depth check, so each holds at most the limit.
    let frontier = MAX_LOCAL_DEFINITIONS.checked_mul(MAX_DEFINITION_CHAIN_DEPTH.checked_add(1)?)?;
    let links = sum(&[
        slots::<std::collections::BTreeSet<(u64, u64)>>(MAX_LOCAL_DEFINITIONS)?,
        set::<(u64, u64)>(MAX_USES)?,
        slots::<std::collections::BTreeSet<usize>>(MAX_LOCAL_DEFINITIONS)?,
        set::<usize>(MAX_INITIALIZER_DEPENDENCIES)?,
        set::<usize>(MAX_LOCAL_DEFINITIONS)?.checked_mul(2)?, // seeds, seen
        set::<(u64, u64)>(MAX_USES)?,                         // one definition's uses
        slots::<(usize, Vec<usize>)>(frontier)?,
        slots::<usize>(frontier.checked_mul(MAX_DEFINITION_CHAIN_DEPTH)?)?,
        slots::<usize>(MAX_DEFINITION_CHAIN_DEPTH.checked_add(1)?)?,
    ])?;
    let validation = sum(&[dto, retained, arena.max(links)])?;
    Some(preflight.max(decode).max(validation))
}
