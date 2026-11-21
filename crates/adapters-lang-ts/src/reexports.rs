//! Barrel re-exports (#995 measurement, AC-1504).
//!
//! A relative import of a directory or barrel module (`import { EventBus }
//! from '../event-bus'`) yields a candidate call target inside the barrel
//! (`…@event-bus/index.ts#EventBus.publish`) although the declaration lives
//! in a file the barrel re-exports. Each file's `export * from './x'` and
//! `export { a as b } from './x'` statements (and `export { a }` of an
//! imported `a`) are recorded here, and directory completion follows them
//! to the declaring file. Resolution is proven against the walk's real
//! symbols and fails closed: two different declarations reachable through
//! `export *` leave the explicit Gap in place, never a pick.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use core_prov::EvidenceRef;

/// Max barrel hops followed for one call. Real barrels nest two or three
/// deep; the bound only stops pathological chains.
const MAX_DEPTH: usize = 8;

/// One re-export statement in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReExport {
    /// Repo-relative path of the re-exporting (barrel) file.
    pub file: String,
    /// Repo-relative target path, with `resolve_relative`'s extension guess.
    pub target: String,
    /// `None` for `export * from`; otherwise `(exported, original)` pairs.
    pub names: Option<Vec<(String, String)>>,
    /// Byte span of the export statement, cited on resolved calls.
    pub span: (u64, u64),
}

/// Re-exports indexed by barrel file.
#[derive(Debug, Default)]
pub(crate) struct Index<'a> {
    by_file: BTreeMap<&'a str, Vec<&'a ReExport>>,
    /// `(file, name)` bindings each file exports itself.
    exported: BTreeSet<(&'a str, &'a str)>,
}

/// A call target proven through one or more barrels.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Resolved {
    pub dst: String,
    /// The export statements crossed, outermost first.
    pub via: Vec<(String, (u64, u64))>,
}

impl<'a> Index<'a> {
    pub(crate) fn new(reexports: &'a [ReExport], exported: &'a [(String, String)]) -> Self {
        let mut by_file: BTreeMap<&str, Vec<&ReExport>> = BTreeMap::new();
        for reexport in reexports {
            by_file.entry(&reexport.file).or_default().push(reexport);
        }
        let exported = exported
            .iter()
            .map(|(file, name)| (file.as_str(), name.as_str()))
            .collect();
        Self { by_file, exported }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.by_file.is_empty()
    }

    /// Follow barrels from `dst` (`sym:{repo}@{path}#{name}[.member]`) to the
    /// one real symbol they re-export, or `None` when no barrel applies, the
    /// chain ends nowhere, or `export *` makes it ambiguous.
    pub(crate) fn resolve(
        &self,
        repo: &str,
        dst: &str,
        known_symbols: &HashSet<String>,
    ) -> Option<Resolved> {
        let rest = dst.strip_prefix(&format!("sym:{repo}@"))?;
        let (path, qualified) = rest.split_once('#')?;
        let (name, member) = match qualified.split_once('.') {
            Some((name, member)) => (name, Some(member)),
            None => (qualified, None),
        };
        let mut found = BTreeSet::new();
        let mut visited = BTreeSet::new();
        self.walk(
            repo,
            path,
            name,
            member,
            known_symbols,
            false,
            &mut Vec::new(),
            &mut visited,
            &mut found,
        );
        let mut found = found.into_iter();
        let first = found.next()?;
        // Fail closed when distinct declarations are reachable.
        found
            .all(|other| other.dst == first.dst)
            .then_some(first)
            .filter(|resolved| !resolved.via.is_empty())
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        repo: &str,
        path: &str,
        name: &str,
        member: Option<&str>,
        known: &HashSet<String>,
        through_star: bool,
        via: &mut Vec<(String, (u64, u64))>,
        visited: &mut BTreeSet<(String, String)>,
        found: &mut BTreeSet<Resolved>,
    ) {
        if via.len() > MAX_DEPTH {
            return;
        }
        for file in file_candidates(repo, path, known) {
            if !visited.insert((file.clone(), name.to_string())) {
                continue;
            }
            let qualified = match member {
                Some(member) => format!("{name}.{member}"),
                None => name.to_string(),
            };
            let candidate = format!("sym:{repo}@{file}#{qualified}");
            // `export *` forwards only what the target file itself exports
            // (or re-exports); a private helper of the same name is no rival.
            if known.contains(&candidate)
                && (!through_star || self.exported.contains(&(file.as_str(), name)))
            {
                found.insert(Resolved {
                    dst: candidate,
                    via: via.clone(),
                });
                continue;
            }
            let Some(reexports) = self.by_file.get(file.as_str()) else {
                continue;
            };
            // ES semantics: an explicit named export shadows every `export *`.
            let named: Vec<_> = reexports
                .iter()
                .filter_map(|reexport| {
                    reexport.names.as_ref().and_then(|names| {
                        names
                            .iter()
                            .find(|(exported, _)| exported == name)
                            .map(|(_, original)| (*reexport, original.as_str()))
                    })
                })
                .collect();
            if !named.is_empty() {
                for (reexport, original) in named {
                    via.push((reexport.file.clone(), reexport.span));
                    self.walk(
                        repo,
                        &reexport.target,
                        original,
                        member,
                        known,
                        false,
                        via,
                        visited,
                        found,
                    );
                    via.pop();
                }
                continue;
            }
            for reexport in reexports.iter().filter(|reexport| reexport.names.is_none()) {
                via.push((reexport.file.clone(), reexport.span));
                self.walk(
                    repo,
                    &reexport.target,
                    name,
                    member,
                    known,
                    true,
                    via,
                    visited,
                    found,
                );
                via.pop();
            }
        }
    }
}

/// The real files a guessed path may name, in the same order the import
/// extension reconciliation tries them.
fn file_candidates(repo: &str, path: &str, known: &HashSet<String>) -> Vec<String> {
    let exists = |candidate: &str| known.contains(&format!("file:{repo}@{candidate}"));
    if exists(path) {
        return vec![path.to_string()];
    }
    let probe = format!("file:{repo}@{path}");
    crate::reconcile_guessed_extension(&probe, known)
        .and_then(|id| {
            id.strip_prefix(&format!("file:{repo}@"))
                .map(str::to_string)
        })
        .into_iter()
        .collect()
}

/// Evidence for each export statement a resolved call crossed.
pub(crate) fn evidence(repo: &str, commit: &str, resolved: &Resolved) -> Vec<EvidenceRef> {
    resolved
        .via
        .iter()
        .map(|(path, (start, end))| EvidenceRef {
            repo: repo.into(),
            path: path.clone(),
            byte_start: *start,
            byte_end: *end,
            commit_sha: commit.into(),
        })
        .collect()
}

#[cfg(test)]
mod tests;
