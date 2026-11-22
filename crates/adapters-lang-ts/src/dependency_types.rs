//! Opt-in dependency type declarations (ADR-0078, AC-1506).
//!
//! A call into a dependency (`send()` imported from `@aws-sdk/client-s3`,
//! `stripe.charges()` on a namespace import) produces no edge at T0: the
//! bare import is an external `mod:` boundary and the call through it is
//! dropped. For packages the author lists in `.cartograph/types.toml`, the
//! package's *declaration* files (never its implementation) are read from
//! the installed `node_modules`, and a call to a member they export becomes a
//! `CALLS` edge to that boundary. Anything not proven stays omitted, as
//! before: this pass adds proof and never adds a Gap.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use core_graph::Edge;
use core_graph::facts::FactAccess;
use core_prov::{EvidenceRef, Provenance};
use serde::Deserialize;

use core_graph::placeholder::Boundary;

use crate::resolution::{self, ResolutionIndex};
use crate::{ExtractError, SourceId};

/// The allowlist, repo-relative (ADR-0078 decision 1).
pub const CONFIG_REL_PATH: &str = ".cartograph/types.toml";
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// Declaration files read per package (ADR-0078 decision 2).
const MAX_FILES: usize = 64;
/// Declaration bytes read per package.
const MAX_PACKAGE_BYTES: u64 = 8 * 1024 * 1024;
/// Re-export hops followed inside one package.
const MAX_DEPTH: usize = 8;

/// One call on a binding imported from a bare specifier, awaiting the
/// allowlist and the package's declarations at directory completion.
#[derive(Debug, Clone)]
pub(crate) struct Call {
    /// The edge to add when proven: caller symbol → `mod:{spec}`, carrying
    /// the call site's provenance.
    pub edge: Edge,
    /// The import specifier, exactly as written.
    pub spec: String,
    /// The exported member the call reaches.
    pub member: String,
    /// Repo-relative path of the calling file.
    pub importer: String,
}

impl Call {
    pub(crate) fn retarget(&mut self, commit: &str) {
        crate::retarget_props_commit(&mut self.edge.props, commit);
    }
}

/// What the reader did on one directory walk (ADR-0078 decision 5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Listed packages whose declarations were read, by package name.
    pub packages_read: Vec<String>,
    /// Listed packages that proved nothing, with the reason.
    pub packages_unavailable: Vec<(String, String)>,
    /// Calls proven as dependency boundaries.
    pub calls_proven: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    read: Vec<String>,
}

/// The packages `root`'s `.cartograph/types.toml` lists. No file is an empty
/// list; a symlinked, non-regular, oversized or malformed file, or an entry
/// that is not an exact package name, is an error, never an empty list.
pub fn allowlist(root: &Path) -> Result<BTreeSet<String>, ExtractError> {
    let refuse = |reason: String| ExtractError::Config(format!("{CONFIG_REL_PATH}: {reason}"));
    let mut path = root.to_path_buf();
    for part in CONFIG_REL_PATH.split('/') {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeSet::new());
            }
            Err(error) => return Err(error.into()),
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(refuse("a symlink is never read".into()));
            }
            Ok(_) => {}
        }
    }
    let meta = std::fs::symlink_metadata(&path)?;
    if !meta.is_file() {
        return Err(refuse("not a regular file".into()));
    }
    if meta.len() > MAX_CONFIG_BYTES {
        return Err(refuse(format!("larger than {MAX_CONFIG_BYTES} bytes")));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|error| refuse(format!("not readable as UTF-8 ({error})")))?;
    let config: Config = toml::from_str(&text).map_err(|error| refuse(error.to_string()))?;
    let mut read = BTreeSet::new();
    for entry in config.read {
        if resolution::package_name(&entry) != Some(entry.as_str()) {
            return Err(refuse(format!(
                "{entry:?} is not an exact package name (`name` or `@scope/name`)"
            )));
        }
        if !read.insert(entry.clone()) {
            return Err(refuse(format!("{entry:?} is listed twice")));
        }
    }
    Ok(read)
}

/// Kinds of exported binding a call can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Function,
    Class,
    Variable,
    Namespace,
    /// Interfaces, type aliases and enums: never a call target.
    Type,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Function => "function",
            Kind::Class => "class",
            Kind::Variable => "variable",
            Kind::Namespace => "namespace",
            Kind::Type => "type",
        }
    }
}

/// One parsed declaration file, package-relative.
#[derive(Debug, Default)]
struct DeclFile {
    /// Names this file exports itself.
    own: BTreeMap<String, Kind>,
    /// `export { a as b } from './x'` and local `export { a as b }`
    /// (target `None`): `(exported, original, target)`.
    named: Vec<(String, String, Option<String>)>,
    /// `export * from './x'` targets.
    stars: Vec<String>,
    /// Top-level declarations, exported or not, for local `export { a }`.
    locals: BTreeMap<String, Kind>,
    /// `export =`: CommonJS declarations, proven nothing in this slice.
    assignment: bool,
}

/// A listed package's declarations, read under the bounds.
#[derive(Debug)]
struct Package {
    version: Option<String>,
    entry: String,
    files: BTreeMap<String, DeclFile>,
}

/// Prove every pending dependency call against its package's declarations.
/// With an empty allowlist nothing under `node_modules` is opened and no
/// fact changes.
pub(crate) fn complete(
    calls: Vec<Call>,
    root: &Path,
    index: &ResolutionIndex,
    id: &SourceId,
    known_files: &HashSet<String>,
    facts: &mut impl FactAccess,
) -> Result<Report, ExtractError> {
    let allow = allowlist(root)?;
    let mut report = Report::default();
    if allow.is_empty() {
        return Ok(report);
    }
    let canonical_root = root.canonicalize()?;
    // One read per (package, installed directory).
    let mut packages: BTreeMap<(String, PathBuf), Result<Package, String>> = BTreeMap::new();
    // One edge per caller and package: the graph keys a relation by
    // `(src, dst, label)`, so every member a caller reaches is kept on it.
    let mut edges: BTreeMap<(String, String), Proven> = BTreeMap::new();
    for call in calls {
        let Some(name) = resolution::package_name(&call.spec) else {
            continue;
        };
        if name != call.spec || !allow.contains(name) {
            continue; // subpath specifiers are not proven in this slice
        }
        // Only a dependency the importer's manifest declares (the same proof
        // as its import boundary).
        let Boundary::External { evidence, .. } =
            resolution::classify_bare_import(&call.spec, &call.importer, index, id, known_files)
        else {
            continue;
        };
        let Some(manifest) = evidence.first() else {
            continue;
        };
        let manifest_dir = Path::new(&manifest.path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let Some(package_dir) = locate(root, &canonical_root, &manifest_dir, name) else {
            packages
                .entry((name.to_string(), PathBuf::new()))
                .or_insert_with(|| Err("not installed under node_modules".into()));
            continue;
        };
        let package = packages
            .entry((name.to_string(), package_dir.clone()))
            .or_insert_with(|| {
                read_package(root, &canonical_root, &manifest_dir, name, &package_dir)
            });
        let Ok(package) = package else {
            continue;
        };
        let Some((kind, file)) =
            package.resolve(&package.entry, &call.member, 0, &mut BTreeSet::new())
        else {
            continue;
        };
        if kind == Kind::Type {
            continue;
        }
        let Ok(mut prov) = serde_json::from_value::<Provenance>(call.edge.props["prov"].clone())
        else {
            continue;
        };
        prov.evidence
            .extend(evidence.iter().cloned().map(|mut cited: EvidenceRef| {
                cited.commit_sha = id.commit.to_string();
                cited
            }));
        report.calls_proven += 1;
        let key = (call.edge.src.clone(), call.edge.dst.clone());
        let proven = edges.entry(key).or_insert_with(|| Proven {
            edge: call.edge,
            version: package.version.clone(),
            members: BTreeMap::new(),
            prov: None,
        });
        proven
            .members
            .insert(call.member.clone(), (kind.as_str(), file));
        match &mut proven.prov {
            None => proven.prov = Some(prov),
            Some(held) => held.evidence.extend(prov.evidence),
        }
    }
    for ((name, _), outcome) in packages {
        match outcome {
            Ok(_) => report.packages_read.push(name),
            Err(reason) => report.packages_unavailable.push((name, reason)),
        }
    }
    report.packages_read.dedup();
    report.packages_unavailable.dedup();
    let edges = edges
        .into_values()
        .map(Proven::into_edge)
        .collect::<Vec<_>>();
    facts.append(Vec::new(), edges)?;
    Ok(report)
}

/// The calls one caller makes into one package, merged into its edge.
struct Proven {
    edge: Edge,
    version: Option<String>,
    /// member -> (declaration kind, package-relative declaring file).
    members: BTreeMap<String, (&'static str, String)>,
    prov: Option<Provenance>,
}

impl Proven {
    fn into_edge(self) -> Edge {
        let mut edge = self.edge;
        edge.props["resolution"] = "dependency-types".into();
        let (members, rest): (Vec<_>, Vec<_>) = self.members.into_iter().unzip();
        let (kinds, files): (Vec<_>, Vec<_>) = rest.into_iter().unzip();
        edge.props["members"] = members.into();
        edge.props["declarations"] = kinds.into();
        edge.props["declared_in"] = files.into();
        if let Some(version) = self.version {
            edge.props["package_version"] = version.into();
        }
        if let Some(mut prov) = self.prov {
            // Call sites in walk order, each manifest line once.
            let mut seen = BTreeSet::new();
            prov.evidence.retain(|cited| {
                seen.insert((cited.path.clone(), cited.byte_start, cited.byte_end))
            });
            edge.props["prov"] = serde_json::to_value(prov).expect("provenance serializes");
        }
        edge
    }
}

/// `@types/<name>`, with a scope folded the DefinitelyTyped way.
fn types_package(name: &str) -> String {
    match name.strip_prefix('@') {
        Some(scoped) => format!("@types/{}", scoped.replacen('/', "__", 1)),
        None => format!("@types/{name}"),
    }
}

/// The installed directory of `name`, searched from the manifest directory
/// up to the repository root the way Node does, falling back to
/// `@types/<name>`; only a directory that resolves inside the root counts.
fn locate(root: &Path, canonical_root: &Path, from: &Path, name: &str) -> Option<PathBuf> {
    locate_one(root, canonical_root, from, name)
        .or_else(|| locate_one(root, canonical_root, from, &types_package(name)))
}

fn locate_one(root: &Path, canonical_root: &Path, from: &Path, name: &str) -> Option<PathBuf> {
    let mut dir = Some(from);
    while let Some(current) = dir {
        let path = root.join(current).join("node_modules").join(name);
        if let Ok(canonical) = path.canonicalize()
            && canonical.starts_with(canonical_root)
            && canonical.join("package.json").is_file()
        {
            return Some(canonical);
        }
        dir = current.parent();
    }
    None
}

fn read_package(
    root: &Path,
    canonical_root: &Path,
    manifest_dir: &Path,
    name: &str,
    package_dir: &Path,
) -> Result<Package, String> {
    let manifest = read_inside(
        canonical_root,
        &package_dir.join("package.json"),
        MAX_MANIFEST_BYTES,
    )?;
    let json: serde_json::Value = serde_json::from_slice(&manifest)
        .map_err(|_| "package.json is not valid JSON".to_string())?;
    let version = json["version"].as_str().map(str::to_string);
    let entry = types_entry(&json)
        .and_then(|entry| declaration_file(package_dir, "", &entry))
        .or_else(|| declaration_file(package_dir, "", "index"));
    let Some(entry) = entry else {
        // A package without types of its own may have `@types/<name>`.
        let types_dir = locate_one(root, canonical_root, manifest_dir, &types_package(name))
            .filter(|dir| dir != package_dir);
        return match types_dir {
            Some(types_dir) => read_package(root, canonical_root, manifest_dir, name, &types_dir),
            None => Err("no type declarations".into()),
        };
    };
    let mut package = Package {
        version,
        entry: entry.clone(),
        files: BTreeMap::new(),
    };
    let mut queue = vec![entry];
    let mut bytes = 0u64;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        .map_err(|error| error.to_string())?;
    while let Some(rel) = queue.pop() {
        if package.files.contains_key(&rel) {
            continue;
        }
        if package.files.len() == MAX_FILES {
            return Err(format!("more than {MAX_FILES} declaration files"));
        }
        let text = read_inside(canonical_root, &package_dir.join(&rel), MAX_PACKAGE_BYTES)?;
        bytes += text.len() as u64;
        if bytes > MAX_PACKAGE_BYTES {
            return Err(format!(
                "more than {MAX_PACKAGE_BYTES} bytes of declarations"
            ));
        }
        let text = String::from_utf8(text).map_err(|_| format!("{rel} is not UTF-8"))?;
        let file = parse(&mut parser, &text, &rel, package_dir)?;
        for target in file.stars.iter().chain(
            file.named
                .iter()
                .filter_map(|(_, _, target)| target.as_ref()),
        ) {
            queue.push(target.clone());
        }
        package.files.insert(rel, file);
    }
    if package.files[&package.entry].assignment {
        return Err("`export =` (CommonJS) declarations".into());
    }
    Ok(package)
}

/// Read `path` only when it resolves inside the repository root.
fn read_inside(canonical_root: &Path, path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let canonical = path
        .canonicalize()
        .map_err(|_| format!("{} is missing", path.display()))?;
    if !canonical.starts_with(canonical_root) {
        return Err("resolves outside the repository".into());
    }
    let meta = std::fs::metadata(&canonical).map_err(|error| error.to_string())?;
    if !meta.is_file() || meta.len() > limit {
        return Err(format!(
            "{} is not a regular file under {limit} bytes",
            path.display()
        ));
    }
    std::fs::read(&canonical).map_err(|error| error.to_string())
}

/// `types`, `typings`, or the `types` condition of the `.` export.
fn types_entry(json: &serde_json::Value) -> Option<String> {
    /// A `types` value: a path, or nested conditions
    /// (`"types": {"import": "./index.d.mts", "require": "./index.d.cts"}`).
    fn declared(value: &serde_json::Value) -> Option<&str> {
        match value {
            serde_json::Value::String(entry) => Some(entry),
            serde_json::Value::Object(map) => ["types", "import", "require", "default"]
                .iter()
                .find_map(|key| map.get(*key).and_then(declared)),
            _ => None,
        }
    }
    fn condition(value: &serde_json::Value) -> Option<&str> {
        match value {
            serde_json::Value::Object(map) => map.get("types").and_then(declared).or_else(|| {
                ["import", "require", "default"]
                    .iter()
                    .find_map(|key| map.get(*key).and_then(condition))
            }),
            _ => None,
        }
    }
    let dot = match &json["exports"] {
        serde_json::Value::Object(map) => map.get(".").or(Some(&json["exports"])),
        _ => None,
    };
    json["types"]
        .as_str()
        .or_else(|| json["typings"].as_str())
        .or_else(|| dot.and_then(condition))
        .map(|entry| entry.trim_start_matches("./").to_string())
}

/// The package-relative declaration file a specifier names from `from_dir`:
/// `x.d.ts`, `x.d.mts`, `x.d.cts`, or `x/index.d.ts`, with a `.js`-style
/// extension mapped to its declaration.
fn declaration_file(package_dir: &Path, from_dir: &str, spec: &str) -> Option<String> {
    let joined = if from_dir.is_empty() {
        spec.to_string()
    } else {
        format!("{from_dir}/{spec}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?; // never above the package directory
            }
            other => parts.push(other),
        }
    }
    let base = parts.join("/");
    if [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|ext| base.ends_with(ext))
    {
        return package_dir.join(&base).is_file().then_some(base);
    }
    let stem = [".js", ".mjs", ".cjs", ".ts", ".mts", ".cts"]
        .iter()
        .find_map(|ext| base.strip_suffix(ext))
        .unwrap_or(&base);
    [
        format!("{stem}.d.ts"),
        format!("{stem}.d.mts"),
        format!("{stem}.d.cts"),
        format!("{stem}/index.d.ts"),
    ]
    .into_iter()
    .find(|candidate| package_dir.join(candidate).is_file())
}

fn parse(
    parser: &mut tree_sitter::Parser,
    text: &str,
    rel: &str,
    package_dir: &Path,
) -> Result<DeclFile, String> {
    let tree = parser
        .parse(text, None)
        .ok_or_else(|| format!("{rel} did not parse"))?;
    let dir = Path::new(rel)
        .parent()
        .map(|dir| dir.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let node_text = |node: tree_sitter::Node<'_>| &text[node.byte_range()];
    let unquote = |node: tree_sitter::Node<'_>| {
        let raw = node_text(node);
        raw.get(1..raw.len().checked_sub(1)?).map(str::to_string)
    };
    let mut file = DeclFile::default();
    let root = tree.root_node();
    let mut cursor = root.walk();
    for stmt in root.named_children(&mut cursor) {
        if stmt.kind() != "export_statement" {
            for (name, kind) in declared(stmt, &node_text) {
                file.locals.insert(name, kind);
            }
            continue;
        }
        let mut walker = stmt.walk();
        let children: Vec<_> = stmt.children(&mut walker).collect();
        if children.iter().any(|child| child.kind() == "default") {
            continue; // default exports are not proven in this slice
        }
        if children.iter().any(|child| child.kind() == "=") {
            file.assignment = true;
            continue;
        }
        if let Some(declaration) = stmt.child_by_field_name("declaration") {
            for (name, kind) in declared(declaration, &node_text) {
                file.locals.insert(name.clone(), kind);
                file.own.insert(name, kind);
            }
            continue;
        }
        let source = stmt.child_by_field_name("source").and_then(unquote);
        // Only relative specifiers stay inside the package.
        let target = match &source {
            Some(spec) if spec.starts_with('.') => declaration_file(package_dir, &dir, spec),
            Some(_) => continue,
            None => None,
        };
        if source.is_some() && target.is_none() {
            continue;
        }
        if let Some(namespace) = children
            .iter()
            .find(|child| child.kind() == "namespace_export")
        {
            // `export * as ns from './x'`
            let mut inner = namespace.walk();
            if let Some(name) = namespace
                .named_children(&mut inner)
                .find(|child| child.kind() == "identifier")
            {
                file.own
                    .insert(node_text(name).to_string(), Kind::Namespace);
            }
            continue;
        }
        if let Some(clause) = children
            .iter()
            .find(|child| child.kind() == "export_clause")
        {
            let mut inner = clause.walk();
            for spec in clause
                .named_children(&mut inner)
                .filter(|spec| spec.kind() == "export_specifier")
            {
                let Some(name) = spec.child_by_field_name("name") else {
                    continue;
                };
                let alias = spec.child_by_field_name("alias").unwrap_or(name);
                file.named.push((
                    node_text(alias).to_string(),
                    node_text(name).to_string(),
                    target.clone(),
                ));
            }
            continue;
        }
        if let Some(target) = target
            && children.iter().any(|child| child.kind() == "*")
        {
            file.stars.push(target);
        }
    }
    Ok(file)
}

/// Names and kinds one top-level declaration binds.
fn declared<'a>(
    node: tree_sitter::Node<'_>,
    text: &impl Fn(tree_sitter::Node<'_>) -> &'a str,
) -> Vec<(String, Kind)> {
    let node = if node.kind() == "ambient_declaration" {
        let mut cursor = node.walk();
        let Some(inner) = node.named_children(&mut cursor).next() else {
            return Vec::new();
        };
        inner
    } else {
        node
    };
    let named = |kind: Kind| {
        node.child_by_field_name("name")
            .filter(|name| matches!(name.kind(), "identifier" | "type_identifier"))
            .map(|name| vec![(text(name).to_string(), kind)])
            .unwrap_or_default()
    };
    match node.kind() {
        "function_signature" | "function_declaration" | "generator_function_declaration" => {
            named(Kind::Function)
        }
        "class_declaration" | "abstract_class_declaration" => named(Kind::Class),
        "internal_module" | "module" => named(Kind::Namespace),
        "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
            named(Kind::Type)
        }
        "lexical_declaration" | "variable_declaration" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .filter(|child| child.kind() == "variable_declarator")
                .filter_map(|declarator| declarator.child_by_field_name("name"))
                .filter(|name| name.kind() == "identifier")
                .map(|name| (text(name).to_string(), Kind::Variable))
                .collect()
        }
        _ => Vec::new(),
    }
}

impl Package {
    /// The kind and declaring file of `name` exported from `file`, following
    /// re-exports with ES semantics: a file's own and named exports shadow
    /// `export *`, and two different declarations through `export *` are
    /// ambiguous, which proves nothing.
    fn resolve(
        &self,
        file: &str,
        name: &str,
        depth: usize,
        visited: &mut BTreeSet<(String, String)>,
    ) -> Option<(Kind, String)> {
        if depth > MAX_DEPTH || !visited.insert((file.to_string(), name.to_string())) {
            return None;
        }
        let decl = self.files.get(file)?;
        if decl.assignment {
            return None;
        }
        if let Some(kind) = decl.own.get(name) {
            return Some((*kind, file.to_string()));
        }
        if let Some((_, original, target)) =
            decl.named.iter().find(|(exported, _, _)| exported == name)
        {
            return match target {
                Some(target) => self.resolve(target, original, depth + 1, visited),
                None => decl
                    .locals
                    .get(original)
                    .map(|kind| (*kind, file.to_string())),
            };
        }
        let mut found: BTreeSet<(Kind, String)> = BTreeSet::new();
        for target in &decl.stars {
            if let Some(hit) = self.resolve(target, name, depth + 1, &mut visited.clone()) {
                found.insert(hit);
            }
        }
        let mut found = found.into_iter();
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }
}

#[cfg(test)]
mod tests;
