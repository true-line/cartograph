//! Inherited members of a typed call (AC-1514).
//!
//! A typed member call proves its receiver's class, `T`, and guesses the
//! target `T.m` in `T`'s file. When `T` declares no `m` but inherits it,
//!
//! ```ts
//! export class DisposableResourceMap<V> extends DisposableMap<URI, V> {}
//! map.get(uri); // DisposableMap.get
//! ```
//!
//! the guess names no Symbol and the call became a Gap. Each class's
//! `extends` clause is recorded here, its base bound the way an import or a
//! same-file class binds a name, and at directory completion the call walks
//! `T`'s chain of bases to the nearest one that declares `m` as an instance
//! method. That is the method the call dispatches to whenever the receiver
//! is a `T`, so the edge stays Confirmed T0, with resolution
//! `inherited-member`, the classes walked under `inherits_via` and every
//! `extends` clause it relied on added to its evidence.
//!
//! A base that is a package import, a namespace member, a call (a mixin) or a
//! global (`Error`) binds nothing, and the walk stops at it. The walk is
//! capped at [`MAX_DEPTH`] bases and stops on a cycle.

use super::{FileCx, TsNode, sym_id};
use core_prov::EvidenceRef;
use std::collections::{HashMap, HashSet};

/// Most bases one call walks.
pub(crate) const MAX_DEPTH: usize = 16;

/// One `class T extends B`: `T`'s and `B`'s guessed class Symbol ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Base {
    pub(crate) class: String,
    pub(crate) base: String,
    pub(crate) evidence: EvidenceRef,
}

/// Name bindings a base's identifier resolves through.
pub(crate) struct Imports<'a> {
    /// local name -> (file, exported name), for relative imports.
    pub(crate) files: &'a HashMap<String, (String, String)>,
    /// local name -> module spec, for every other import.
    pub(crate) modules: &'a HashMap<String, String>,
}

/// The `extends` clause of every named class in one file.
pub(crate) fn bases(
    cx: &FileCx<'_>,
    nodes: &[TsNode<'_>],
    imports: &Imports<'_>,
    out: &mut Vec<Base>,
) {
    for class in nodes {
        if !matches!(
            class.kind(),
            "class_declaration" | "abstract_class_declaration"
        ) {
            continue;
        }
        let Some(name) = class
            .child_by_field_name("name")
            .filter(|n| n.kind() == "type_identifier")
        else {
            continue;
        };
        let mut walk = class.walk();
        let Some(clause) = class
            .named_children(&mut walk)
            .find(|n| n.kind() == "class_heritage")
            .and_then(|heritage| {
                let mut walk = heritage.walk();
                heritage
                    .named_children(&mut walk)
                    .find(|n| n.kind() == "extends_clause")
            })
        else {
            continue;
        };
        let Some(value) = clause
            .child_by_field_name("value")
            .filter(|v| v.kind() == "identifier")
        else {
            continue;
        };
        let local = cx.text(&value);
        let base = match imports.files.get(local) {
            Some((file, exported)) => sym_id(cx.id.repo, file, exported),
            None if imports.modules.contains_key(local) => continue,
            None => sym_id(cx.id.repo, cx.path, local),
        };
        out.push(Base {
            class: sym_id(cx.id.repo, cx.path, cx.text(&name)),
            base,
            evidence: EvidenceRef {
                repo: cx.id.repo.into(),
                path: cx.path.into(),
                byte_start: clause.start_byte() as u64,
                byte_end: clause.end_byte() as u64,
                commit_sha: cx.id.commit.into(),
            },
        });
    }
}

/// Re-cite cached bases at the revision ingested now.
pub(crate) fn retarget(bases: &mut [Base], commit: &str) {
    for base in bases {
        base.evidence.commit_sha = commit.to_string();
    }
}

/// Every recorded class's base, keyed by the class's Symbol id.
#[derive(Debug, Default)]
pub(crate) struct Index {
    bases: HashMap<String, (String, EvidenceRef)>,
}

impl Index {
    /// `resolve` maps a guessed base id to the Symbol id it names (extension
    /// reconciliation, barrel re-exports); a base it cannot place is kept as
    /// guessed and simply never matches a method.
    pub(crate) fn new(bases: Vec<Base>, resolve: impl Fn(&str) -> Option<String>) -> Self {
        let mut index = HashMap::new();
        for Base {
            class,
            base,
            evidence,
        } in bases
        {
            let base = resolve(&base).unwrap_or(base);
            // Two declarations of one class name in one file (merged or
            // conditional) leave the base unproven.
            match index.entry(class) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(Some((base, evidence)));
                }
                std::collections::hash_map::Entry::Occupied(mut slot) => {
                    slot.insert(None);
                }
            }
        }
        Self {
            bases: index
                .into_iter()
                .filter_map(|(class, base)| Some((class, base?)))
                .collect(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.bases.is_empty()
    }

    /// The candidate targets for a guessed `…#T.m`, nearest base first: each
    /// `B.m` with the classes walked to reach it and the `extends` clauses
    /// cited. `place` maps the guessed receiver class to the Symbol id it
    /// names, as [`Index::new`] did for bases. Only candidates in `known`
    /// are returned.
    pub(crate) fn chain(
        &self,
        dst: &str,
        place: impl Fn(&str) -> Option<String>,
        known: &HashSet<String>,
    ) -> Vec<Inherited> {
        let Some((prefix, qualified)) = dst.rsplit_once('#') else {
            return Vec::new();
        };
        let Some((class, method)) = qualified.split_once('.') else {
            return Vec::new();
        };
        let guessed = format!("{prefix}#{class}");
        let mut current = place(&guessed).unwrap_or(guessed);
        let mut seen = HashSet::from([current.clone()]);
        let mut via = vec![current.clone()];
        let mut evidence = Vec::new();
        let mut found = Vec::new();
        for _ in 0..MAX_DEPTH {
            let Some((base, cited)) = self.bases.get(&current) else {
                break;
            };
            if !seen.insert(base.clone()) {
                break;
            }
            evidence.push(cited.clone());
            via.push(base.clone());
            let candidate = format!("{base}.{method}");
            if known.contains(&candidate) {
                found.push(Inherited {
                    dst: candidate,
                    via: via.clone(),
                    evidence: evidence.clone(),
                });
            }
            current = base.clone();
        }
        found
    }
}

/// One inherited declaration a typed call may reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Inherited {
    pub(crate) dst: String,
    /// Class Symbol ids from the receiver's class to the declaring base.
    pub(crate) via: Vec<String>,
    pub(crate) evidence: Vec<EvidenceRef>,
}
