//! Typed calls on an interface reach its implementations (AC-1515).
//!
//! A typed member call whose receiver is proven to be an interface `I`
//! guesses `I.m` in `I`'s file. An interface member is a signature, not a
//! Symbol, so the call became a Gap. The classic case is a cross-process
//! proxy:
//!
//! ```ts
//! // extHost.protocol.ts
//! export interface ExtHostUrlsShape { $handleExternalUri(uri: URI): Promise<void>; }
//! // mainThreadUrls.ts
//! this._proxy = context.getProxy(ExtHostContext.ExtHostUrls); // ExtHostUrlsShape
//! this._proxy.$handleExternalUri(uri);
//! // extHostUrls.ts
//! export class ExtHostUrls implements ExtHostUrlsShape { $handleExternalUri(uri) { … } }
//! ```
//!
//! Each class's `implements` clause and each interface's `extends` clause
//! are recorded here, bound the way an import or a same-file declaration
//! binds a name. At directory completion a typed call on `I` is linked to
//! `m` on every class that implements `I`, directly or through an
//! interface that extends `I`, and on every subclass of such a class: for
//! each, the nearest declaration of `m` in its own chain of bases
//! ([`heritage`](super::heritage)). The edge is `CALLS` with resolution
//! `interface-implementation`, InferredStrong (which class answers is a
//! runtime fact), naming `interface` and `implementation` and counting the
//! `alternatives`, and citing the call and each clause relied on.
//!
//! Product code never reaches an implementation declared in test code, and
//! test code reaches one only when nothing else implements the interface: a
//! mock never stands beside the real class. An interface
//! with more than [`MAX_ALTERNATIVES`] distinct targets links nothing, and
//! the call keeps its Gap.

use super::{FileCx, TsNode, heritage, resolve_relative, sym_id};
use core_prov::EvidenceRef;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Most implementations one call is linked to.
pub(crate) const MAX_ALTERNATIVES: usize = 8;

/// Most interfaces, and most subclasses, one lookup walks.
const MAX_WALK: usize = 64;

/// One class's `implements` clause, interface ids bound.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Implements {
    class: String,
    interfaces: Vec<String>,
    evidence: EvidenceRef,
}

/// One interface's `extends` clause, parent ids bound.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Extends {
    interface: String,
    parents: Vec<String>,
    evidence: EvidenceRef,
}

/// One file's clauses, awaiting the directory.
#[derive(Debug, Clone, Default)]
pub(crate) struct Pending {
    implements: Vec<Implements>,
    extends: Vec<Extends>,
    /// Every interface declared, by its own file's id: a clause or a call
    /// may name it by a guessed extension.
    declared: Vec<String>,
}

impl Pending {
    pub(crate) fn extend(&mut self, other: Self) {
        self.implements.extend(other.implements);
        self.extends.extend(other.extends);
        self.declared.extend(other.declared);
    }

    pub(crate) fn retarget(&mut self, commit: &str) {
        for clause in &mut self.implements {
            clause.evidence.commit_sha = commit.to_string();
        }
        for clause in &mut self.extends {
            clause.evidence.commit_sha = commit.to_string();
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.implements.is_empty()
    }

    /// Every interface declared, by its own file's id, so placement can
    /// find it. Clause ids are guesses and are left out: a guess listed as
    /// known would never be placed.
    pub(crate) fn interfaces(&self) -> impl Iterator<Item = &str> {
        self.declared.iter().map(String::as_str)
    }
}

/// The interface ids a heritage clause's types name.
fn named(cx: &FileCx<'_>, clause: TsNode<'_>, imports: &heritage::Imports<'_>) -> Vec<String> {
    let mut walk = clause.walk();
    let mut out = Vec::new();
    for ty in clause.named_children(&mut walk) {
        let ty = match ty.kind() {
            "generic_type" => match ty.child_by_field_name("name") {
                Some(name) => name,
                None => continue,
            },
            _ => ty,
        };
        let id = match ty.kind() {
            "type_identifier" => imports.bind(cx, cx.text(&ty)),
            // `implements protocol.Shape` through `import * as protocol`
            // of a relative file.
            "nested_type_identifier" => ty
                .child_by_field_name("module")
                .filter(|module| module.kind() == "identifier")
                .zip(ty.child_by_field_name("name"))
                .and_then(|(module, name)| {
                    let spec = imports.modules.get(cx.text(&module))?;
                    let file = resolve_relative(cx.path, spec)?;
                    Some(sym_id(cx.id.repo, &file, cx.text(&name)))
                }),
            _ => None,
        };
        out.extend(id);
    }
    out
}

fn evidence(cx: &FileCx<'_>, clause: TsNode<'_>) -> EvidenceRef {
    EvidenceRef {
        repo: cx.id.repo.into(),
        path: cx.path.into(),
        byte_start: clause.start_byte() as u64,
        byte_end: clause.end_byte() as u64,
        commit_sha: cx.id.commit.into(),
    }
}

/// Every named class's `implements` clause and interface's `extends`
/// clause in one file.
pub(crate) fn clauses(
    cx: &FileCx<'_>,
    nodes: &[TsNode<'_>],
    imports: &heritage::Imports<'_>,
    out: &mut Pending,
) {
    for node in nodes {
        let Some(name) = node
            .child_by_field_name("name")
            .filter(|n| n.kind() == "type_identifier")
        else {
            continue;
        };
        let id = sym_id(cx.id.repo, cx.path, cx.text(&name));
        match node.kind() {
            "class_declaration" | "abstract_class_declaration" => {
                let mut walk = node.walk();
                let Some(clause) = node
                    .named_children(&mut walk)
                    .find(|n| n.kind() == "class_heritage")
                    .and_then(|heritage| {
                        let mut walk = heritage.walk();
                        heritage
                            .named_children(&mut walk)
                            .find(|n| n.kind() == "implements_clause")
                    })
                else {
                    continue;
                };
                let interfaces = named(cx, clause, imports);
                if !interfaces.is_empty() {
                    out.implements.push(Implements {
                        class: id,
                        interfaces,
                        evidence: evidence(cx, clause),
                    });
                }
            }
            "interface_declaration" => {
                out.declared.push(id.clone());
                let mut walk = node.walk();
                let Some(clause) = node
                    .named_children(&mut walk)
                    .find(|n| n.kind() == "extends_type_clause")
                else {
                    continue;
                };
                let parents = named(cx, clause, imports);
                if !parents.is_empty() {
                    out.extends.push(Extends {
                        interface: id,
                        parents,
                        evidence: evidence(cx, clause),
                    });
                }
            }
            _ => {}
        }
    }
}

/// One implementation a typed interface call may reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Implementation {
    pub(crate) dst: String,
    pub(crate) interface: String,
    /// The class whose instance answers the call.
    pub(crate) class: String,
    pub(crate) evidence: Vec<EvidenceRef>,
}

/// Every class implementing each interface, with the `implements` clause
/// and each interface `extends` clause that say so.
#[derive(Debug, Default)]
pub(crate) struct Index {
    implementers: HashMap<String, Vec<(String, Vec<EvidenceRef>)>>,
}

impl Index {
    /// `place` maps a guessed id to the id it names, as for bases.
    pub(crate) fn new(pending: Pending, place: impl Fn(&str) -> Option<String>) -> Self {
        let place = |id: String| place(&id).unwrap_or(id);
        let mut parents: HashMap<String, Vec<(String, EvidenceRef)>> = HashMap::new();
        for clause in pending.extends {
            let evidence = clause.evidence;
            parents.entry(place(clause.interface)).or_default().extend(
                clause
                    .parents
                    .into_iter()
                    .map(|parent| (place(parent), evidence.clone())),
            );
        }
        let mut implementers: HashMap<String, Vec<(String, Vec<EvidenceRef>)>> = HashMap::new();
        for clause in pending.implements {
            let class = place(clause.class);
            // Each named interface and every interface it extends, breadth
            // first so each is reached by its shortest chain of clauses.
            let mut queue: std::collections::VecDeque<(String, Vec<EvidenceRef>)> = clause
                .interfaces
                .into_iter()
                .map(|interface| (place(interface), vec![clause.evidence.clone()]))
                .collect();
            let mut seen = HashSet::new();
            while let Some((interface, cited)) = queue.pop_front() {
                if seen.len() >= MAX_WALK || !seen.insert(interface.clone()) {
                    continue;
                }
                for (parent, extends) in parents.get(&interface).into_iter().flatten() {
                    let mut chain = cited.clone();
                    chain.push(extends.clone());
                    queue.push_back((parent.clone(), chain));
                }
                implementers
                    .entry(interface)
                    .or_default()
                    .push((class.clone(), cited));
            }
        }
        Self { implementers }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.implementers.is_empty()
    }

    /// The candidate targets of a guessed `…#I.m`: for each implementing
    /// class and each of its subclasses, the nearest `m` in known.
    pub(crate) fn implementations(
        &self,
        dst: &str,
        bases: &heritage::Index,
        place: impl Fn(&str) -> Option<String> + Copy,
        known: &HashSet<String>,
    ) -> Vec<Implementation> {
        let Some((prefix, qualified)) = dst.rsplit_once('#') else {
            return Vec::new();
        };
        let Some((interface, method)) = qualified.split_once('.') else {
            return Vec::new();
        };
        let guessed = format!("{prefix}#{interface}");
        let interface = place(&guessed).unwrap_or(guessed);
        let Some(classes) = self.implementers.get(&interface) else {
            return Vec::new();
        };
        // Keyed by target and class: a mock and a product class may answer
        // with one inherited method, and only [`choose`] tells them apart.
        let mut found: BTreeMap<(String, String), Implementation> = BTreeMap::new();
        for (implementer, clauses) in classes {
            for (class, subclass) in bases.descendants(implementer, MAX_WALK) {
                let mut evidence = clauses.clone();
                evidence.extend(subclass);
                let own = format!("{class}.{method}");
                let target = if known.contains(&own) {
                    Some((own, Vec::new()))
                } else {
                    bases
                        .chain(&own, place, known)
                        .into_iter()
                        .next()
                        .map(|inherited| (inherited.dst, inherited.evidence))
                };
                if let Some((target, chain)) = target {
                    evidence.extend(chain);
                    found
                        .entry((target.clone(), class.clone()))
                        .or_insert(Implementation {
                            dst: target,
                            interface: interface.clone(),
                            class,
                            evidence,
                        });
                }
            }
        }
        found.into_values().collect()
    }
}

/// The file path inside a Symbol id `sym:repo@path#name`.
pub(crate) fn symbol_path(id: &str) -> &str {
    id.split_once('@')
        .and_then(|(_, rest)| rest.split_once('#'))
        .map_or("", |(path, _)| path)
}

/// Product code never reaches an implementation declared in test code;
/// test code reaches one only when nothing outside tests implements the
/// interface. More than [`MAX_ALTERNATIVES`] targets link nothing.
pub(crate) fn choose(
    mut found: Vec<Implementation>,
    site: &str,
    in_test: impl Fn(&str) -> bool,
) -> Vec<Implementation> {
    // A class declared in test code is a mock even when the method it
    // answers with is inherited from product code.
    let test = |found: &Implementation| in_test(symbol_path(&found.class));
    if !in_test(symbol_path(site)) || found.iter().any(|found| !test(found)) {
        found.retain(|found| !test(found));
    }
    let mut targets = HashSet::new();
    found.retain(|found| targets.insert(found.dst.clone()));
    if found.len() > MAX_ALTERNATIVES {
        found.clear();
    }
    found
}
