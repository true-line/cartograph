//! Rule-bearing source suggestions (#1728, AC-1433).
//!
//! The language adapters recover guarded exits and effects as `BusinessRule`
//! facts owned by the enclosing callable. This module ranks the callables that
//! own them, so a caller choosing source obligations can start from the code
//! that holds validation and refusal rules rather than from small helpers. The
//! ranking reads graph metadata only: no source text, model or heuristic.
use crate::AppState;
use agents::TaskFactKey;
use core_graph::rules::{GuardedExitEvidence, LocalExit, RuleKind};
use core_graph::{GraphStore, Node};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use tauri::Manager;

/// Most suggestions one call returns.
pub(crate) const MAX_RULE_SOURCES: usize = 64;
/// Most suggestions taken from one file, so one large class cannot fill the list.
pub(crate) const MAX_PER_FILE: usize = 2;

/// One callable and the recovered rules it owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RuleBearingOwner {
    pub owner_id: String,
    pub repo: String,
    pub path: String,
    /// Guarded exits whose effect is a throw.
    pub guarded_throws: u32,
    /// Every guarded exit, throws included.
    pub guarded_exits: u32,
    /// Guarded effects: state changes and unused calls.
    pub guarded_effects: u32,
}

/// Rank the owners of `rules` (nodes labelled `BusinessRule`): most guarded
/// throws first, then most guarded exits, then most guarded effects, then path
/// and owner id. At most `per_file` owners come from one file. With `files`,
/// only rules whose exit lies in one of those repository paths count. A rule
/// whose payload does not decode is skipped, never guessed.
pub(crate) fn rank_owners(
    rules: &[Node],
    files: Option<&BTreeSet<String>>,
    per_file: usize,
) -> Vec<RuleBearingOwner> {
    let mut owners: BTreeMap<String, RuleBearingOwner> = BTreeMap::new();
    for node in rules.iter().filter(|node| node.label == "BusinessRule") {
        let Ok(rule) = GuardedExitEvidence::from_value(node.props["rule"].clone()) else {
            continue;
        };
        let source = &rule.exit_source;
        if files.is_some_and(|files| !files.contains(&source.path)) {
            continue;
        }
        let owner = owners
            .entry(rule.owner_id.clone())
            .or_insert_with(|| RuleBearingOwner {
                owner_id: rule.owner_id.clone(),
                repo: source.repo.clone(),
                path: source.path.clone(),
                guarded_throws: 0,
                guarded_exits: 0,
                guarded_effects: 0,
            });
        // An owner's rules share its file; a payload naming another one is
        // inconsistent and does not move the owner.
        if owner.repo != source.repo || owner.path != source.path {
            continue;
        }
        match rule.kind {
            RuleKind::GuardedExit => {
                owner.guarded_exits = owner.guarded_exits.saturating_add(1);
                if matches!(rule.effect, LocalExit::Throw { .. }) {
                    owner.guarded_throws = owner.guarded_throws.saturating_add(1);
                }
            }
            RuleKind::GuardedEffect => {
                owner.guarded_effects = owner.guarded_effects.saturating_add(1);
            }
        }
    }
    let mut ranked: Vec<_> = owners.into_values().collect();
    ranked.sort_by(|a, b| {
        b.guarded_throws
            .cmp(&a.guarded_throws)
            .then(b.guarded_exits.cmp(&a.guarded_exits))
            .then(b.guarded_effects.cmp(&a.guarded_effects))
            .then_with(|| (&a.repo, &a.path, &a.owner_id).cmp(&(&b.repo, &b.path, &b.owner_id)))
    });
    let mut taken: BTreeMap<(String, String), usize> = BTreeMap::new();
    ranked.retain(|owner| {
        let count = taken
            .entry((owner.repo.clone(), owner.path.clone()))
            .or_default();
        *count += 1;
        *count <= per_file
    });
    ranked
}

/// A ranked owner with the captured identity a source obligation needs.
#[derive(Debug, Serialize)]
pub(crate) struct RuleBearingSource {
    pub fact: TaskFactKey,
    #[serde(flatten)]
    pub owner: RuleBearingOwner,
    pub graph_snapshot_id: String,
    pub receipt_id: String,
    /// The first captured range of the owner, as `inventory_index` for
    /// `resolve_investigation_source_obligation`.
    pub range_index: usize,
}

/// The ranked owners that have a captured source, at most `limit`. An owner
/// without a bound capture receipt is skipped, so every entry resolves.
pub(crate) fn rule_bearing_sources(
    state: &AppState,
    files: Option<&BTreeSet<String>>,
    limit: usize,
) -> Result<Vec<RuleBearingSource>, String> {
    let limit = limit.min(MAX_RULE_SOURCES);
    let rules = state
        .graph
        .lock()
        .map_err(|_| "Graph store is unavailable.".to_owned())?
        .nodes_with_label("BusinessRule")
        .map_err(|error| error.to_string())?;
    let mut sources = Vec::new();
    for owner in rank_owners(&rules, files, MAX_PER_FILE) {
        if sources.len() >= limit {
            break;
        }
        let fact = TaskFactKey::Node {
            id: owner.owner_id.clone(),
        };
        let Ok(description) =
            crate::primary_source::describe(state, &crate::task_evidence::graph_fact(&fact))
        else {
            continue;
        };
        let Some(range) = description.ranges.first() else {
            continue;
        };
        sources.push(RuleBearingSource {
            fact,
            owner,
            graph_snapshot_id: description.graph_snapshot_id,
            receipt_id: description.receipt_id,
            range_index: range.index,
        });
    }
    Ok(sources)
}

#[tauri::command]
pub(crate) async fn investigation_rule_bearing_sources(
    files: Option<Vec<String>>,
    limit: usize,
    app: tauri::AppHandle,
) -> Result<Vec<RuleBearingSource>, String> {
    crate::off_ui_thread(move || {
        let files = files.map(|files| files.into_iter().collect::<BTreeSet<_>>());
        rule_bearing_sources(&app.state::<AppState>(), files.as_ref(), limit)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule(id: &str, owner: &str, path: &str, kind: &str, effect: &str) -> Node {
        use core_graph::rules::*;
        let source = |start: u64, end: u64| core_prov::EvidenceRef {
            repo: "o/r".into(),
            path: path.into(),
            byte_start: start,
            byte_end: end,
            commit_sha: "c".into(),
        };
        let expression = SourceExpression {
            source: source(24, 40),
            syntax_kind: "binary_expression".into(),
            display: SanitizedDisplay::from_sanitized("ready !== false".into()),
            capture: ExpressionCapture::CompleteSyntax,
            literal: None,
        };
        let rule = GuardedExitEvidence {
            schema_version: 1,
            local_definitions: None,
            kind: if kind == "effect" {
                RuleKind::GuardedEffect
            } else {
                RuleKind::GuardedExit
            },
            owner_id: owner.into(),
            exit_source: source(100, 113),
            source_order: 0,
            conditions: vec![BranchCondition {
                branch_source: source(20, 140),
                expression: expression.clone(),
                polarity: BranchPolarity::TruthyBranch,
            }],
            effect: match effect {
                "throw" => LocalExit::Throw { value: expression },
                "return" => LocalExit::Return { value: None },
                _ => LocalExit::Call { value: expression },
            },
            dependencies: vec![],
            interpretation: Interpretation {
                execution_predicate: InterpretationStatus::NotEstablished,
                consumer_effect: InterpretationStatus::NotEstablished,
                gap_ids: vec!["gap:rule:consumer".into()],
            },
            redactions: vec![],
        };
        rule.validate().unwrap();
        Node {
            id: id.into(),
            label: "BusinessRule".into(),
            props: json!({ "rule": rule }),
        }
    }

    #[test]
    fn rule_owners_rank_by_throws_then_exits_and_cap_each_file() {
        let rules = vec![
            rule("r1", "helper", "A.java", "exit", "return"),
            rule("r2", "validate", "B.java", "exit", "throw"),
            rule("r3", "validate", "B.java", "exit", "throw"),
            rule("r4", "guard", "B.java", "exit", "throw"),
            rule("r5", "third", "B.java", "exit", "throw"),
            rule("r6", "effects", "C.java", "effect", "call"),
            rule("r7", "outside", "D.java", "exit", "throw"),
            Node {
                id: "bad".into(),
                label: "BusinessRule".into(),
                props: json!({"rule": {"schema_version": 99}}),
            },
        ];
        let files: BTreeSet<String> = ["A.java", "B.java", "C.java"]
            .into_iter()
            .map(String::from)
            .collect();
        let ranked = rank_owners(&rules, Some(&files), 2);
        let order: Vec<_> = ranked.iter().map(|o| o.owner_id.as_str()).collect();
        // validate (2 throws) > guard (1 throw, B.java before third) > helper
        // (1 exit) > effects; `third` is the third B.java owner, `outside` is
        // not in the closure, and the undecodable payload is skipped.
        assert_eq!(order, ["validate", "guard", "helper", "effects"]);
        assert_eq!(
            (
                ranked[0].guarded_throws,
                ranked[0].guarded_exits,
                ranked[0].guarded_effects
            ),
            (2, 2, 0)
        );
        assert_eq!(ranked[3].guarded_effects, 1);
        let all = rank_owners(&rules, None, 2);
        assert_eq!(all.first().map(|o| o.owner_id.as_str()), Some("validate"));
        assert!(all.iter().any(|o| o.owner_id == "outside"));
    }
}
