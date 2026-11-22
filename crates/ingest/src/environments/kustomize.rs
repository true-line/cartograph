//! Kustomization files and what each reaches (#1791, AC-0662, ADR-0070
//! decision 8, SPEC-14 increment 2).
//!
//! Every `kustomization.yaml`, `kustomization.yml` or `Kustomization` is an
//! environment. It declares the workloads in the manifest files its
//! `resources` and `bases` entries name, and in the kustomization
//! directories they name, followed inside the repository.
//!
//! **Paths are resolved lexically** against the kustomization's directory and
//! looked up among the files the walk found. Nothing outside the walk is
//! opened, no symbolic link is followed (the walk refuses them), and an entry
//! that leaves the repository is a Gap.
//!
//! **Nothing is applied.** A patch file is read as manifest documents and
//! each one that names a reached workload is reported with what it writes.
//! Transformers and generators are named and not evaluated; the literals of
//! a generator are never read.
//!
//! **Bounds.** A chain of kustomizations is followed 16 deep. Within one
//! environment a kustomization is expanded once, so the work is linear in
//! the entries the environment reaches.

use super::kubernetes::{self, Document};
use super::{
    Draft, Findings, MAX_MANIFEST_PROBE_BYTES, Props, find_kind, manifest_candidate, mapping_of,
    names_an_environment, not_read,
};
use crate::pipelines::{Span, field, fields, items, sanitize, span_of};
use crate::schedules::{first_mapping, scalar};
use source_walk::WalkedFile;
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::{Node as TsNode, Parser};

/// The names kustomize reads a kustomization from, in its order.
pub(super) const FILE_NAMES: [&str; 3] =
    ["kustomization.yaml", "kustomization.yml", "Kustomization"];

/// How many kustomizations deep a `resources` chain is followed.
pub(super) const MAX_DEPTH: usize = 16;

/// Transformers and generators that are named and not evaluated.
const UNEVALUATED: &[&str] = &[
    "namePrefix",
    "nameSuffix",
    "commonLabels",
    "labels",
    "replicas",
    "configMapGenerator",
    "secretGenerator",
    "components",
    "helmCharts",
    "generators",
    "transformers",
    "replacements",
    "vars",
];

/// The keys of an `images` entry that are stored, as written.
const IMAGE_KEYS: &[&str] = &["name", "newName", "newTag", "digest"];

/// One `resources`, `bases` or patch entry that names a path.
#[derive(Debug)]
pub(super) struct Entry {
    key: &'static str,
    value: String,
    span: Span,
}

/// One kustomization file.
#[derive(Debug)]
pub(super) struct Kustomization {
    /// `name` and `name_span` where `namespace` is stated, `image_overrides`.
    pub(super) props: Props,
    /// The `resources` key, or the file's first key.
    pub(super) span: Span,
    entries: Vec<Entry>,
    patches: Vec<Entry>,
    /// Patch files applied by a `target` selector: read as nothing, and
    /// still patch files.
    selected: Vec<String>,
    findings: Findings,
    /// The transformer and generator keys present, in [`UNEVALUATED`] order.
    pub(super) unevaluated: Vec<&'static str>,
}

/// True when `value` is patch text and not a path.
fn inline(value: &str) -> bool {
    value.contains('\n') || value.contains(": ") || value.trim_start().starts_with('{')
}

/// A kustomization file, or why it yields no `Environment`.
pub(super) fn parse(text: &str) -> Result<Kustomization, (&'static str, Span)> {
    let whole = (0, text.len() as u64);
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_yaml::LANGUAGE.into())
        .map_err(|_| ("the YAML grammar is unavailable", whole))?;
    let tree = parser
        .parse(text, None)
        .ok_or(("the file is not valid YAML", whole))?;
    let root = tree.root_node();
    if root.has_error() {
        return Err(("the file is not valid YAML", whole));
    }
    let mut walk = root.walk();
    if root
        .named_children(&mut walk)
        .filter(|node| node.kind() == "document")
        .count()
        != 1
    {
        return Err(("the file is not one YAML document", whole));
    }
    let not_mapping = ("the file is not a mapping", whole);
    let top = first_mapping(root).ok_or(not_mapping)?;
    let top = fields(text, top)
        .map_err(|_| ("the file is not a mapping of distinct literal keys", whole))?;
    let span = field(&top, "resources")
        .or(top.first())
        .ok_or(not_mapping)?
        .key_span;

    let mut draft = Draft::default();
    let mut findings = Findings::default();
    if let Some(namespace) = field(&top, "namespace") {
        match namespace.value.and_then(|value| scalar(text, value)) {
            Some(value) => {
                draft.text("name", &value);
                draft.set("name_span", vec![namespace.span.0, namespace.span.1]);
            }
            None => findings.add(not_read("namespace"), namespace.span),
        }
    }
    if let Some(images) = field(&top, "images") {
        let read = images.value.and_then(items).and_then(|entries| {
            entries
                .into_iter()
                .map(|entry| image(text, entry, &mut draft))
                .collect::<Option<Vec<serde_json::Value>>>()
        });
        match read {
            Some(entries) if entries.is_empty() => {}
            Some(entries) => draft.set("image_overrides", entries),
            None => findings.add(not_read("images"), images.span),
        }
    }

    let mut entries = Vec::new();
    for key in ["resources", "bases"] {
        let Some(found) = field(&top, key) else {
            continue;
        };
        let Some(list) = found.value.and_then(items) else {
            findings.add(not_read(key), found.span);
            continue;
        };
        for item in list {
            match scalar(text, item) {
                Some(value) => entries.push(Entry {
                    key,
                    value,
                    span: span_of(item),
                }),
                None => findings.add(
                    format!("a `{key}` entry is not a plain scalar"),
                    span_of(item),
                ),
            }
        }
    }

    let mut patches = Vec::new();
    let mut selected = Vec::new();
    const INLINE: &str = "an inline patch is not read";
    if let Some(found) = field(&top, "patchesStrategicMerge") {
        match found.value.and_then(items) {
            Some(list) => {
                for item in list {
                    match scalar(text, item) {
                        Some(value) if !inline(&value) => patches.push(Entry {
                            key: "patchesStrategicMerge",
                            value,
                            span: span_of(item),
                        }),
                        Some(_) => findings.add(INLINE, span_of(item)),
                        None if find_kind(item, "block_scalar").is_some() => {
                            findings.add(INLINE, span_of(item));
                        }
                        None => findings.add(not_read("patchesStrategicMerge"), span_of(item)),
                    }
                }
            }
            None => findings.add(not_read("patchesStrategicMerge"), found.span),
        }
    }
    if let Some(found) = field(&top, "patches") {
        match found.value.and_then(items) {
            Some(list) => {
                for item in list {
                    patch(text, item, &mut patches, &mut selected, &mut findings);
                }
            }
            None => findings.add(not_read("patches"), found.span),
        }
    }
    if let Some(found) = field(&top, "patchesJson6902") {
        findings.add("`patchesJson6902` is not read", found.key_span);
    }
    let mut unevaluated = Vec::new();
    for key in UNEVALUATED {
        // The key alone: a generator's entry holds literals.
        if let Some(found) = field(&top, key) {
            unevaluated.push(*key);
            findings.add(
                format!("`{key}` is present and not evaluated"),
                found.key_span,
            );
        }
    }
    Ok(Kustomization {
        props: draft.finish(),
        span,
        entries,
        patches,
        selected,
        findings,
        unevaluated,
    })
}

/// One `images` entry as written: the known keys it states.
fn image(text: &str, entry: TsNode, draft: &mut Draft) -> Option<serde_json::Value> {
    let body = fields(text, mapping_of(entry)?).ok()?;
    let mut out = Props::new();
    for key in IMAGE_KEYS {
        if let Some(found) = field(&body, key) {
            let value = scalar(text, found.value?)?;
            out.insert((*key).into(), draft.safe("image_overrides", &value).into());
        }
    }
    Some(serde_json::Value::Object(out))
}

/// One `patches` entry: a path (as a string, or under `path`), an inline
/// patch, or a patch applied by a `target` selector.
fn patch(
    text: &str,
    item: TsNode,
    patches: &mut Vec<Entry>,
    selected: &mut Vec<String>,
    findings: &mut Findings,
) {
    let span = span_of(item);
    if let Some(value) = scalar(text, item) {
        if inline(&value) {
            findings.add("an inline patch is not read", span);
        } else {
            patches.push(Entry {
                key: "patches",
                value,
                span,
            });
        }
        return;
    }
    let Some(body) = mapping_of(item).and_then(|body| fields(text, body).ok()) else {
        findings.add(not_read("patches"), span);
        return;
    };
    // A selector decides what the patch applies to; matching its documents
    // by kind and name would state something the file does not.
    if let Some(target) = field(&body, "target") {
        findings.add(
            "a `patches` entry with a `target` selector is not evaluated",
            target.key_span,
        );
        selected.extend(field(&body, "path").and_then(|path| scalar(text, path.value?)));
        return;
    }
    if let Some(stated) = field(&body, "patch") {
        findings.add("an inline patch is not read", stated.key_span);
    }
    match field(&body, "path") {
        Some(path) => match path.value.and_then(|value| scalar(text, value)) {
            Some(value) => patches.push(Entry {
                key: "patches",
                value,
                span,
            }),
            None => findings.add(not_read("patches"), path.span),
        },
        None if field(&body, "patch").is_none() => findings.add(not_read("patches"), span),
        None => {}
    }
}

/// What a YAML file holds, as far as this reader looked.
#[derive(Debug)]
pub(super) enum Manifest {
    /// Over the probe bound; not opened.
    TooLarge,
    /// Not readable text, or not valid YAML.
    Unparsed,
    /// Its manifest documents; none when it holds none.
    Documents(Vec<Document>),
}

/// What an entry of a kustomization names.
enum Target<'t> {
    File(&'t str),
    Kustomization(&'t str),
    Problem(&'static str),
}

/// The files one walk found, with every kustomization and manifest parsed.
pub(super) struct Tree<'a> {
    files: BTreeMap<&'a str, &'a WalkedFile>,
    /// Every directory that holds a walked file, the root as "".
    dirs: BTreeSet<&'a str>,
    kustomizations: BTreeMap<&'a str, Result<Kustomization, (&'static str, Span)>>,
    manifests: BTreeMap<&'a str, Manifest>,
    /// Files some kustomization names as a patch and none as a resource.
    patch_only: BTreeSet<&'a str>,
}

fn dir_of(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// True for a reference kustomize would fetch.
fn remote(entry: &str) -> bool {
    entry.contains("://")
        || entry.starts_with("git@")
        || entry.starts_with("git::")
        || entry.contains("?ref=")
        || entry.contains("?version=")
        || ["github.com/", "gitlab.com/", "bitbucket.org/"]
            .iter()
            .any(|host| entry.starts_with(host))
}

/// `entry` relative to `dir`, normalised without touching the file system.
/// `None` when it is absolute or leaves the repository.
fn normalise(dir: &str, entry: &str) -> Option<String> {
    if entry.starts_with('/') || entry.starts_with('~') || entry.contains('\\') {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|part| !part.is_empty()).collect();
    for part in entry.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

impl<'a> Tree<'a> {
    /// Parse every kustomization of the walk, every YAML file that may hold a
    /// manifest, and every file a kustomization names.
    pub(super) fn read(walked: &'a [WalkedFile], src: &super::Source) -> Self {
        let mut tree = Self {
            files: walked
                .iter()
                .map(|file| (file.rel.as_str(), file))
                .collect(),
            dirs: BTreeSet::from([""]),
            kustomizations: BTreeMap::new(),
            manifests: BTreeMap::new(),
            patch_only: BTreeSet::new(),
        };
        for file in walked {
            let mut dir = dir_of(&file.rel);
            while tree.dirs.insert(dir) {
                dir = dir_of(dir);
            }
            if FILE_NAMES.contains(&file.name.as_str()) {
                let parsed = match src.text(&file.rel) {
                    Ok(text) => parse(&text),
                    Err(_) => Err((
                        "the file is not readable UTF-8 text",
                        (0, src.len_or_zero(&file.rel)),
                    )),
                };
                tree.kustomizations.insert(&file.rel, parsed);
            } else if let Some(manifest) = load(file, false, src) {
                tree.manifests.insert(&file.rel, manifest);
            }
        }
        // What the kustomizations name: a file no rule selected is opened to
        // say why it yields nothing.
        let mut resources = BTreeSet::new();
        let mut patches = BTreeSet::new();
        let mut named = Vec::new();
        for (rel, kustomization) in &tree.kustomizations {
            let Ok(kustomization) = kustomization else {
                continue;
            };
            for (entries, into) in [
                (&kustomization.entries, &mut resources),
                (&kustomization.patches, &mut patches),
            ] {
                for entry in entries {
                    if let Target::File(file) = tree.resolve(dir_of(rel), &entry.value) {
                        into.insert(file);
                        named.push(file);
                    }
                }
            }
            for path in &kustomization.selected {
                if let Target::File(file) = tree.resolve(dir_of(rel), path) {
                    patches.insert(file);
                }
            }
        }
        for rel in named {
            if !tree.manifests.contains_key(rel)
                && let Some(manifest) = load(tree.files[rel], true, src)
            {
                tree.manifests.insert(rel, manifest);
            }
        }
        tree.patch_only = patches.difference(&resources).copied().collect();
        tree
    }

    fn resolve(&self, dir: &str, entry: &str) -> Target<'a> {
        if remote(entry) {
            return Target::Problem("is a remote reference, which is not fetched");
        }
        let Some(path) = normalise(dir, entry) else {
            return Target::Problem("is a path outside the repository");
        };
        if let Some((rel, _)) = self.files.get_key_value(path.as_str()) {
            return Target::File(rel);
        }
        if !self.dirs.contains(path.as_str()) {
            return Target::Problem(
                "names no file or kustomization directory among the files read",
            );
        }
        let found = FILE_NAMES.iter().find_map(|name| {
            let rel = if path.is_empty() {
                (*name).to_string()
            } else {
                format!("{path}/{name}")
            };
            self.kustomizations.get_key_value(rel.as_str())
        });
        match found {
            Some((rel, Ok(_))) => Target::Kustomization(rel),
            Some((_, Err(_))) => Target::Problem("names a kustomization file that is not read"),
            None => Target::Problem("names a directory that holds no kustomization file"),
        }
    }

    /// The kustomization at `rel`, or why it yields no `Environment`.
    pub(super) fn kustomization(
        &self,
        rel: &str,
    ) -> Option<&Result<Kustomization, (&'static str, Span)>> {
        self.kustomizations.get(rel)
    }

    /// The manifest documents of `rel` that yield facts: none for a file
    /// that is read only as a patch.
    pub(super) fn documents(&self, rel: &str) -> &[Document] {
        match self.manifests.get(rel) {
            Some(Manifest::Documents(documents)) if !self.patch_only.contains(rel) => documents,
            _ => &[],
        }
    }

    /// The documents of `rel` as an entry of a kustomization names them, or
    /// why the file yields nothing.
    fn named(&self, rel: &str) -> Result<&[Document], &'static str> {
        match self.manifests.get(rel) {
            Some(Manifest::Documents(documents)) if !documents.is_empty() => Ok(documents),
            Some(Manifest::TooLarge) => Err("names a file over 1 MiB, which is not opened"),
            Some(Manifest::Unparsed) => Err("names a file that is not valid YAML"),
            _ => Err("names a file that holds no manifest document"),
        }
    }

    /// Everything the kustomization at `rel` reaches and patches.
    pub(super) fn reach(&self, rel: &'a str, kustomization: &Kustomization) -> Reach<'a> {
        let mut walk = Walk {
            tree: self,
            stack: vec![rel],
            done: BTreeMap::new(),
            out: Reach {
                includes: Vec::new(),
                declares: BTreeMap::new(),
                patches: BTreeMap::new(),
                findings: kustomization.findings.clone(),
            },
        };
        walk.follow(rel, kustomization, None, &mut Vec::new());
        let mut out = walk.out;
        self.patches(rel, kustomization, &mut out);
        out
    }

    fn patches(&self, rel: &str, kustomization: &Kustomization, out: &mut Reach<'a>) {
        for entry in &kustomization.patches {
            let problem = |out: &mut Reach<'a>, why: &str| {
                out.findings.add(
                    format!("`{}` entry `{}` {why}", entry.key, sanitize(&entry.value).0),
                    entry.span,
                );
            };
            let file = match self.resolve(dir_of(rel), &entry.value) {
                Target::File(file) => file,
                Target::Kustomization(_) => {
                    problem(out, "names a directory, not a patch file");
                    continue;
                }
                Target::Problem(why) => {
                    problem(out, why);
                    continue;
                }
            };
            let documents = match self.named(file) {
                Ok(documents) => documents,
                Err(why) => {
                    problem(out, why);
                    continue;
                }
            };
            for document in documents {
                let Some(read) = &document.read else {
                    out.findings.add(
                        match kubernetes::plain_kind(&document.kind) {
                            Some(kind) => format!(
                                "a patch document in `{file}` is a `{kind}`, a kind that is not \
                                 read"
                            ),
                            None => format!(
                                "a patch document in `{file}` is of a kind that is not read"
                            ),
                        },
                        entry.span,
                    );
                    continue;
                };
                let Some(name) = &read.name else {
                    out.findings.add(
                        format!("a patch document in `{file}` has no plain scalar `metadata.name`"),
                        entry.span,
                    );
                    continue;
                };
                for reason in &read.findings.reasons {
                    out.findings
                        .add(format!("patch `{file}`: {reason}"), entry.span);
                }
                let matched: Vec<(&'a str, usize)> = out
                    .declares
                    .keys()
                    .copied()
                    .filter(|(path, index)| {
                        self.documents(path)
                            .iter()
                            .find(|candidate| candidate.index == *index)
                            .is_some_and(|candidate| {
                                candidate.kind == document.kind
                                    && candidate
                                        .read
                                        .as_ref()
                                        .is_some_and(|read| read.name.as_ref() == Some(name))
                            })
                    })
                    .collect();
                if matched.is_empty() {
                    out.findings.add(
                        format!(
                            "patch document `{}/{name}` in `{file}` matches no workload this \
                             environment reaches",
                            document.kind
                        ),
                        entry.span,
                    );
                }
                // What the patch writes, in the workload's property shape.
                let mut props = Props::new();
                for key in ["replicas", "containers"] {
                    if let Some(value) = read.props.get(key) {
                        props.insert(key.into(), value.clone());
                    }
                }
                for key in matched {
                    let statements = out.patches.entry(key).or_default();
                    if !statements
                        .iter()
                        .any(|known| known.path == file && known.span == document.span)
                    {
                        statements.push(Statement {
                            path: file,
                            span: document.span,
                            props: props.clone(),
                        });
                    }
                }
            }
        }
    }
}

/// The manifest state of one YAML file. On the walk (`named` false) a file
/// that cannot hold a manifest is not kept; a file a kustomization names is
/// always classified, so the kustomization can say why it yields nothing.
fn load(file: &WalkedFile, named: bool, src: &super::Source) -> Option<Manifest> {
    if !manifest_candidate(&file.name, &file.rel) || names_an_environment(&file.name) {
        return None;
    }
    if src.len(&file.rel).ok()? > MAX_MANIFEST_PROBE_BYTES {
        return named.then_some(Manifest::TooLarge);
    }
    let Ok(text) = src.text(&file.rel) else {
        return named.then_some(Manifest::Unparsed);
    };
    if kubernetes::may_hold_manifest(&text) {
        return Some(match kubernetes::documents(&text) {
            Some(documents) => Manifest::Documents(documents),
            None => Manifest::Unparsed,
        });
    }
    named.then(|| match kubernetes::documents(&text) {
        // Not a manifest by the rule the walk applies.
        Some(_) => Manifest::Documents(Vec::new()),
        None => Manifest::Unparsed,
    })
}

/// How one environment reaches one workload.
#[derive(Debug)]
pub(super) struct Declared<'a> {
    /// The kustomizations between the environment and the file.
    pub(super) via: Vec<&'a str>,
    /// The entries of the environment's own file that lead to it.
    pub(super) spans: BTreeSet<Span>,
}

/// What one patch document writes for one workload.
#[derive(Debug)]
pub(super) struct Statement<'a> {
    /// The patch file.
    pub(super) path: &'a str,
    /// The patch document.
    pub(super) span: Span,
    /// `replicas` and `containers`, each only where the patch states it.
    pub(super) props: Props,
}

/// What one kustomization reaches. Workloads are keyed by their file and the
/// index of their document.
#[derive(Debug)]
pub(super) struct Reach<'a> {
    /// The kustomization files its own entries name, in file order.
    pub(super) includes: Vec<&'a str>,
    pub(super) declares: BTreeMap<(&'a str, usize), Declared<'a>>,
    /// In the order the kustomization lists its patches.
    pub(super) patches: BTreeMap<(&'a str, usize), Vec<Statement<'a>>>,
    /// The file's own findings and those of the walk.
    pub(super) findings: Findings,
}

struct Walk<'t, 'a> {
    tree: &'t Tree<'a>,
    /// The kustomizations being expanded, the environment's own first.
    stack: Vec<&'a str>,
    /// The workloads each expanded kustomization reached.
    done: BTreeMap<&'a str, Vec<(&'a str, usize)>>,
    out: Reach<'a>,
}

impl<'a> Walk<'_, 'a> {
    /// Follow the entries of the kustomization at `rel`. `top` is the entry
    /// of the environment's own file this branch started from; `None` while
    /// reading that file itself.
    fn follow(
        &mut self,
        rel: &'a str,
        kustomization: &Kustomization,
        top: Option<Span>,
        chain: &mut Vec<&'a str>,
    ) -> Vec<(&'a str, usize)> {
        let tree = self.tree;
        let mut reached = Vec::new();
        for entry in &kustomization.entries {
            let span = top.unwrap_or(entry.span);
            let problem = |walk: &mut Self, why: &str| {
                if top.is_none() {
                    walk.out.findings.add(
                        format!("`{}` entry `{}` {why}", entry.key, sanitize(&entry.value).0),
                        span,
                    );
                } else {
                    // The entry is in another kustomization, whose own Gap
                    // states it.
                    walk.out.findings.add(
                        format!(
                            "`{rel}` has entries that are not followed, so what is reached \
                             through it is incomplete"
                        ),
                        span,
                    );
                }
            };
            match tree.resolve(dir_of(rel), &entry.value) {
                Target::Problem(why) => problem(self, why),
                Target::File(file) => match tree.named(file) {
                    Err(why) => problem(self, why),
                    Ok(_) => {
                        for document in tree.documents(file) {
                            if document
                                .read
                                .as_ref()
                                .is_some_and(|read| read.name.is_some())
                            {
                                let key = (file, document.index);
                                self.record(key, chain, span);
                                reached.push(key);
                            }
                        }
                    }
                },
                Target::Kustomization(next) => {
                    if top.is_none() && !self.out.includes.contains(&next) {
                        self.out.includes.push(next);
                    }
                    if self.stack.contains(&next) {
                        self.out.findings.add(
                            format!(
                                "the `resources` chain returns to `{next}`, a cycle, and is not \
                                 followed further"
                            ),
                            span,
                        );
                    } else if chain.len() >= MAX_DEPTH {
                        self.out.findings.add(
                            format!(
                                "the `resources` chain is deeper than {MAX_DEPTH} \
                                 kustomizations, so `{next}` is not followed"
                            ),
                            span,
                        );
                    } else if let Some(known) = self.done.get(next).cloned() {
                        // Expanded once; this entry leads to the same workloads.
                        for key in known {
                            self.record(key, chain, span);
                            reached.push(key);
                        }
                    } else if let Some(Ok(inner)) = tree.kustomizations.get(next) {
                        self.stack.push(next);
                        chain.push(next);
                        let under = self.follow(next, inner, Some(span), chain);
                        chain.pop();
                        self.stack.pop();
                        self.done.insert(next, under.clone());
                        reached.extend(under);
                    }
                }
            }
        }
        reached
    }

    /// The first chain found is the one stated; every entry of the
    /// environment's own file that leads to the workload is cited.
    fn record(&mut self, key: (&'a str, usize), chain: &[&'a str], span: Span) {
        self.out
            .declares
            .entry(key)
            .or_insert_with(|| Declared {
                via: chain.to_vec(),
                spans: BTreeSet::new(),
            })
            .spans
            .insert(span);
    }
}
