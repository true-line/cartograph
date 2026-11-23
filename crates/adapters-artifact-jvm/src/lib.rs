//! JVM class-file artifact reader (ADR-0051 §5 slice 8, #1445).
//!
//! A deterministic, static read of compiled `.class` files: each class and
//! method is a `Symbol`, each class file a `File`, each call instruction
//! between methods of the read classes a `CALLS` edge, and each referenced
//! class outside the class's own package and `java.lang` an `IMPORTS` edge.
//! Every fact is T0 `Confirmed` with an `artifact` evidence basis: the class
//! file's `sha256:` digest, kind `jvm-class` and the byte span it was read
//! from (AC-1215). Nothing is executed, verified or decompiled. A class file
//! the reader cannot walk whole is one `Gap` naming why.

pub mod class_file;

use class_file::{
    ACC_ABSTRACT, ACC_ANNOTATION, ACC_BRIDGE, ACC_ENUM, ACC_INTERFACE, ACC_STATIC, ACC_SYNTHETIC,
    Call, ClassFile, MemberRef, Method, Span,
};
use core_graph::{Edge, Node};
use core_prov::{ArtifactLocator, ArtifactRef, ConfidenceTier, EvidenceBasis, Provenance, Tier};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Provenance extractor id of every fact this reader emits.
pub const EXTRACTOR_ID: &str = "t0.artifact-jvm";

/// Artifact kind recorded on every evidence reference.
pub const ARTIFACT_KIND: &str = "jvm-class";

/// Largest class file read; a larger one is a Gap.
pub const MAX_CLASS_BYTES: usize = 8 * 1024 * 1024;

/// Most synthetic bodies followed from one method (lambdas in lambdas).
const MAX_SYNTHETIC_DEPTH: usize = 8;

/// Identity of the source being recovered.
pub struct SourceId<'a> {
    /// Repository identity.
    pub repo: &'a str,
    /// Commit SHA, or `workdir` for an unversioned tree.
    pub commit: &'a str,
}

/// Graph facts read from a set of class files.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Extraction {
    /// Recovered nodes.
    pub nodes: Vec<Node>,
    /// Recovered edges.
    pub edges: Vec<Edge>,
}

fn file_id(repo: &str, path: &str) -> String {
    format!("file:{repo}@{path}")
}

fn symbol_id(repo: &str, path: &str, name: &str) -> String {
    format!("sym:{repo}@{path}#{name}")
}

/// `sha256:` and the lowercase hex digest of `bytes`.
pub fn digest(bytes: &[u8]) -> String {
    hex_digest(&Sha256::digest(bytes))
}

fn hex_digest(hash: &[u8]) -> String {
    let mut out = String::with_capacity(71);
    out.push_str("sha256:");
    for byte in hash {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// A class file as read for extraction: its bytes, or the length and
/// digest of one larger than [`MAX_CLASS_BYTES`], which is never held whole.
enum Content {
    Bytes(Vec<u8>),
    TooLarge { digest: String, len: u64 },
}

impl Content {
    fn of(bytes: Vec<u8>) -> Content {
        if bytes.len() > MAX_CLASS_BYTES {
            Content::TooLarge {
                digest: digest(&bytes),
                len: bytes.len() as u64,
            }
        } else {
            Content::Bytes(bytes)
        }
    }
}

/// Read a class file of the checkout holding at most [`MAX_CLASS_BYTES`]
/// and one more byte: a larger file is streamed through its digest.
fn read_class_file(path: &std::path::Path) -> std::io::Result<Content> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_CLASS_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() <= MAX_CLASS_BYTES {
        return Ok(Content::Bytes(bytes));
    }
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let mut len = bytes.len() as u64;
    drop(bytes);
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        len += n as u64;
    }
    Ok(Content::TooLarge {
        digest: hex_digest(&hasher.finalize()),
        len,
    })
}

fn gap(id: &SourceId<'_>, path: &str, code: &str, digest: &str, len: u64) -> Node {
    let gap = format!("gap:{}@{path}#jvm-class", id.repo);
    Node {
        id: gap.clone(),
        label: "Gap".into(),
        props: serde_json::json!({
            "path": path,
            "reason": format!("unreadable JVM class file: {code}"),
            "reason_code": code,
            "attempted_tiers": ["T0"],
            "prov": prov(digest, (0, len), ConfidenceTier::Gap, &format!("Gap {gap}")),
        }),
    }
}

fn prov(digest: &str, span: Span, confidence: ConfidenceTier, fact: &str) -> serde_json::Value {
    let provenance = Provenance::new(
        Tier::Deterministic,
        confidence,
        Vec::new(),
        EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("Deterministic confidence is within its ceiling")
    .with_basis(EvidenceBasis::Artifact {
        refs: vec![ArtifactRef {
            digest: digest.to_string(),
            kind: ARTIFACT_KIND.into(),
            at: ArtifactLocator::Offset {
                start: span.0,
                end: span.1,
            },
        }],
    });
    serde_json::to_value(provenance).expect("provenance serializes")
}

/// The class's name below its package, nested classes joined by `.`
/// (`Ledger.Store`), or `None` for an anonymous or local class (a segment
/// after `$` that starts with a digit).
fn nested_name(internal: &str) -> Option<String> {
    let simple = internal.rsplit('/').next().unwrap_or(internal);
    let segments: Vec<&str> = simple.split('$').collect();
    if segments
        .iter()
        .any(|segment| segment.is_empty() || segment.starts_with(|c: char| c.is_ascii_digit()))
    {
        return None;
    }
    Some(segments.join("."))
}

fn package_of(internal: &str) -> &str {
    internal.rsplit_once('/').map_or("", |(package, _)| package)
}

fn dotted(internal: &str) -> String {
    internal.replace(['/', '$'], ".")
}

fn class_kind(access: u16) -> &'static str {
    if access & ACC_ANNOTATION != 0 {
        "annotation_type"
    } else if access & ACC_INTERFACE != 0 {
        "interface"
    } else if access & ACC_ENUM != 0 {
        "enum"
    } else {
        "class"
    }
}

/// How many leading constructor parameters the compiler added that the
/// source does not declare: an inner (non-static member) class's enclosing
/// instance, or an enum constant's name and ordinal.
fn added_constructor_parameters(class: &ClassFile, descriptor: &str) -> usize {
    if class.access & ACC_ENUM != 0
        && class.super_class.as_deref() == Some("java/lang/Enum")
        && descriptor.starts_with("(Ljava/lang/String;I")
    {
        return 2;
    }
    match &class.nesting {
        Some(nesting)
            if nesting.access & (ACC_STATIC | ACC_INTERFACE | ACC_ENUM) == 0
                && class.access & ACC_INTERFACE == 0 =>
        {
            match &nesting.outer {
                Some(outer) if descriptor.starts_with(&format!("(L{outer};")) => 1,
                _ => 0,
            }
        }
        _ => 0,
    }
}

/// A method's parameters as its source declares them, spelled simply or
/// fully qualified: a constructor drops the parameters the compiler added.
fn source_parameters(
    class: &ClassFile,
    method: &str,
    descriptor: &str,
    qualified: bool,
) -> Option<Vec<String>> {
    let mut params = if qualified {
        class_file::qualified_parameter_types(descriptor)?
    } else {
        class_file::parameter_types(descriptor)?
    };
    if method == "<init>" {
        let added = added_constructor_parameters(class, descriptor).min(params.len());
        params.drain(..added);
    }
    Some(params)
}

/// The qualified name of each of the class's methods, index for index, as
/// the Java source adapter spells it: `Owner.method(Type,Type)`, a
/// constructor named for its class and listing only the parameters its
/// source declares. Overloads whose simple spellings collide
/// (`java.util.Date` and `java.sql.Date`) spell their classes in full, so
/// each method keeps its own identity. `None` for a method not emitted.
fn method_names(owner: &str, class: &ClassFile) -> Vec<Option<String>> {
    let simple: Vec<Option<Vec<String>>> = class
        .methods
        .iter()
        .map(|method| {
            emitted(method)
                .then(|| source_parameters(class, &method.name, &method.descriptor, false))
                .flatten()
        })
        .collect();
    let mut spellings: BTreeMap<(&str, &[String]), usize> = BTreeMap::new();
    for (method, params) in class.methods.iter().zip(&simple) {
        if let Some(params) = params {
            *spellings
                .entry((method.name.as_str(), params.as_slice()))
                .or_default() += 1;
        }
    }
    let short = owner.rsplit('.').next().unwrap_or(owner);
    class
        .methods
        .iter()
        .zip(&simple)
        .map(|(method, params)| {
            let params = params.as_ref()?;
            let params = if spellings[&(method.name.as_str(), params.as_slice())] > 1 {
                source_parameters(class, &method.name, &method.descriptor, true)?
            } else {
                params.clone()
            };
            let name = if method.name == "<init>" {
                short
            } else {
                &method.name
            };
            Some(format!("{owner}.{name}({})", params.join(",")))
        })
        .collect()
}

/// A method this reader emits: not a static initializer, bridge or other
/// compiler-generated method.
fn emitted(method: &Method) -> bool {
    method.name != "<clinit>" && method.access & (ACC_SYNTHETIC | ACC_BRIDGE) == 0
}

struct Read<'a> {
    path: &'a str,
    digest: String,
    class: ClassFile,
    nested: String,
    /// [`method_names`] of the class.
    names: Vec<Option<String>>,
}

/// Read `files` — `(repository-relative path, bytes)` pairs of `.class`
/// files — into graph facts. Output is ordered by path, so the same files
/// in any order give the same extraction.
pub fn extract(id: &SourceId<'_>, files: &[(String, Vec<u8>)]) -> Extraction {
    extract_with(id, files, &[])
}

/// [`extract`], plus a Gap for each `(path, digest, length)` of a class
/// file too large to read.
fn extract_with(
    id: &SourceId<'_>,
    files: &[(String, Vec<u8>)],
    too_large: &[(String, String, u64)],
) -> Extraction {
    let mut out = Extraction::default();
    for (path, digest, len) in too_large {
        out.nodes.push(gap(id, path, "too_large", digest, *len));
    }
    let mut ordered: Vec<&(String, Vec<u8>)> = files.iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0));
    let mut reads: Vec<Read<'_>> = Vec::new();
    for (path, bytes) in ordered {
        let digest = digest(bytes);
        let parsed = if bytes.len() > MAX_CLASS_BYTES {
            Err("too_large")
        } else {
            class_file::parse(bytes).map_err(|refusal| refusal.code())
        };
        match parsed {
            Ok(class) => {
                // An anonymous or local class has no name to cite; its
                // calls are not attributed (AC-1215).
                if let Some(nested) = nested_name(&class.this_class) {
                    let names = method_names(&nested, &class);
                    reads.push(Read {
                        path,
                        digest,
                        class,
                        nested,
                        names,
                    });
                }
            }
            Err(code) => {
                out.nodes
                    .push(gap(id, path, code, &digest, bytes.len() as u64));
            }
        }
    }

    // Every emitted method of the read classes, by (owner, name, descriptor),
    // then by the path of each file that declares it: a checkout may ship
    // the same binary class more than once.
    let mut methods: Methods<'_> = BTreeMap::new();
    for read in &reads {
        for (method, name) in read.class.methods.iter().zip(&read.names) {
            if let Some(name) = name {
                methods
                    .entry((
                        read.class.this_class.clone(),
                        method.name.clone(),
                        method.descriptor.clone(),
                    ))
                    .or_default()
                    .insert(read.path, symbol_id(id.repo, read.path, name));
            }
        }
    }

    for read in &reads {
        emit_class(id, read, &methods, &mut out);
    }
    out
}

/// Method symbols by (owner, name, descriptor), then by declaring path.
type Methods<'a> = BTreeMap<(String, String, String), BTreeMap<&'a str, String>>;

fn emit_class(id: &SourceId<'_>, read: &Read<'_>, methods: &Methods<'_>, out: &mut Extraction) {
    let Read {
        path,
        digest,
        class,
        nested,
        names,
    } = read;
    let file = file_id(id.repo, path);
    out.nodes.push(Node {
        id: file.clone(),
        label: "File".into(),
        props: serde_json::json!({
            "path": path,
            "language": "jvm-class",
            "artifact_digest": digest,
            "prov": prov(digest, class.header_span, ConfidenceTier::Confirmed, &format!("File {path}")),
        }),
    });
    let symbol = symbol_id(id.repo, path, nested);
    let mut props = serde_json::json!({
        "name": nested,
        "kind": class_kind(class.access),
        "language": "java",
        "binary_name": class.this_class.replace('/', "."),
        "abstract": class.access & ACC_ABSTRACT != 0 && class.access & ACC_INTERFACE == 0,
        "prov": prov(digest, class.header_span, ConfidenceTier::Confirmed, &format!("Symbol {symbol}")),
    });
    if let Some(super_class) = class
        .super_class
        .as_deref()
        .filter(|s| *s != "java/lang/Object")
    {
        props["extends"] = dotted(super_class).into();
    }
    if !class.interfaces.is_empty() {
        props["implements"] = class
            .interfaces
            .iter()
            .map(|i| dotted(i))
            .collect::<Vec<_>>()
            .into();
    }
    out.nodes.push(Node {
        id: symbol.clone(),
        label: "Symbol".into(),
        props,
    });
    out.edges.push(Edge {
        src: symbol.clone(),
        dst: file.clone(),
        label: "DEFINED_IN".into(),
        props: serde_json::json!({
            "prov": prov(digest, class.header_span, ConfidenceTier::Confirmed, &format!("DEFINED_IN {symbol}")),
        }),
    });

    // Synthetic methods of this class, for lambda bodies.
    let synthetic: BTreeMap<(&str, &str), &Method> = class
        .methods
        .iter()
        .filter(|method| method.access & ACC_SYNTHETIC != 0)
        .map(|method| ((method.name.as_str(), method.descriptor.as_str()), method))
        .collect();

    for (method, name) in class.methods.iter().zip(names) {
        let Some(name) = name else {
            continue;
        };
        let source = symbol_id(id.repo, path, name);
        out.nodes.push(Node {
            id: source.clone(),
            label: "Symbol".into(),
            props: serde_json::json!({
                "name": name,
                "kind": if method.name == "<init>" { "constructor" } else { "method" },
                "language": "java",
                "descriptor": method.descriptor,
                "prov": prov(digest, method.span, ConfidenceTier::Confirmed, &format!("Symbol {source}")),
            }),
        });
        out.edges.push(Edge {
            src: source.clone(),
            dst: file.clone(),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": prov(digest, method.span, ConfidenceTier::Confirmed, &format!("DEFINED_IN {source}")),
            }),
        });
        let mut seen = BTreeSet::new();
        let mut calls = Vec::new();
        collect_calls(
            class,
            &synthetic,
            method,
            None,
            0,
            &mut BTreeSet::new(),
            &mut calls,
        );
        for (target, opcode, span, via) in calls {
            let key = (
                target.owner.clone(),
                target.name.clone(),
                target.descriptor.clone(),
            );
            // A call into this file's own class names this file's method;
            // a call into a class shipped more than once names no one copy.
            let Some(dst) = methods.get(&key).and_then(|paths| {
                paths
                    .get(&**path)
                    .or_else(|| (paths.len() == 1).then(|| paths.values().next()).flatten())
            }) else {
                continue;
            };
            if !seen.insert((dst.clone(), span)) {
                continue;
            }
            let mut props = serde_json::json!({
                "opcode": opcode,
                "prov": prov(digest, span, ConfidenceTier::Confirmed, &format!("CALLS {source} {dst} {}", span.0)),
            });
            if let Some(via) = via {
                props["via"] = via.into();
            }
            out.edges.push(Edge {
                src: source.clone(),
                dst: dst.clone(),
                label: "CALLS".into(),
                props,
            });
        }
    }

    let own_package = package_of(&class.this_class);
    let mut imported = BTreeSet::new();
    // Classes the constant pool names, then classes named only in a field
    // or method descriptor (a parameter, return or field type).
    let pooled = class.class_refs.iter().filter_map(|reference| {
        // An array type names its element class after the `[`s and `L`.
        let name = reference.name.trim_start_matches('[');
        let name = match name.strip_prefix('L') {
            Some(element) if reference.name.starts_with('[') => element.trim_end_matches(';'),
            _ if reference.name.starts_with('[') => return None,
            _ => name,
        };
        Some((name, reference.span))
    });
    let described = class
        .fields
        .iter()
        .map(|field| (field.descriptor.as_str(), field.span))
        .chain(
            class
                .methods
                .iter()
                .map(|method| (method.descriptor.as_str(), method.span)),
        )
        .flat_map(|(descriptor, span)| {
            class_file::descriptor_classes(descriptor)
                .into_iter()
                .map(move |name| (name, span))
        });
    for (name, span) in pooled.chain(described) {
        let package = package_of(name);
        let outer = name.split('$').next().unwrap_or(name);
        if outer == class.this_class.split('$').next().unwrap_or("")
            || package == own_package
            || package == "java/lang"
            || nested_name(name).is_none()
        {
            continue;
        }
        let module = dotted(name);
        if !imported.insert(module.clone()) {
            continue;
        }
        out.edges.push(Edge {
            src: file.clone(),
            dst: format!("mod:{module}"),
            label: "IMPORTS".into(),
            props: serde_json::json!({
                "specifier": module,
                "prov": prov(digest, span, ConfidenceTier::Confirmed, &format!("IMPORTS {path} {module}")),
            }),
        });
    }
}

type Found = (MemberRef, &'static str, Span, Option<&'static str>);

/// The calls `method` makes, following the bodies of this class's synthetic
/// methods its `invokedynamic` instructions name (a lambda's calls are its
/// enclosing method's, `via: lambda`).
fn collect_calls(
    class: &ClassFile,
    synthetic: &BTreeMap<(&str, &str), &Method>,
    method: &Method,
    via: Option<&'static str>,
    depth: usize,
    visited: &mut BTreeSet<(String, String)>,
    out: &mut Vec<Found>,
) {
    for call in &method.calls {
        match call {
            Call::Invoke {
                opcode,
                target,
                span,
            } => out.push((target.clone(), opcode, *span, via)),
            Call::Dynamic { handles, span } => {
                for handle in handles {
                    let body = (handle.owner == class.this_class)
                        .then(|| synthetic.get(&(handle.name.as_str(), handle.descriptor.as_str())))
                        .flatten();
                    let Some(body) = body else {
                        // A method reference (`store::amount`) names the
                        // method it calls.
                        out.push((
                            handle.clone(),
                            "invokedynamic",
                            *span,
                            Some("method_reference"),
                        ));
                        continue;
                    };
                    if depth >= MAX_SYNTHETIC_DEPTH {
                        continue;
                    }
                    if !visited.insert((handle.name.clone(), handle.descriptor.clone())) {
                        continue;
                    }
                    collect_calls(
                        class,
                        synthetic,
                        body,
                        Some("lambda"),
                        depth + 1,
                        visited,
                        out,
                    );
                }
            }
        }
    }
}

/// A directory the walk does not enter: hidden, dependency or build output
/// (a build's classes come from sources the source adapters read).
fn skipped_dir(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "target" | "build" | "out" | "node_modules" | "dist" | "generated"
        )
}

/// Every path suffix, at `/` boundaries, of each Java or Kotlin source path
/// without its extension: `src/com/acme/Pay.java` gives `com/acme/Pay`,
/// `acme/Pay` and `Pay` among others.
fn source_stems(paths: &[String]) -> BTreeSet<String> {
    let mut stems = BTreeSet::new();
    for path in paths {
        let Some(stem) = path
            .strip_suffix(".java")
            .or_else(|| path.strip_suffix(".kt"))
            .or_else(|| path.strip_suffix(".kts"))
        else {
            continue;
        };
        stems.insert(stem.to_string());
        for (at, _) in stem.match_indices('/') {
            stems.insert(stem[at + 1..].to_string());
        }
    }
    stems
}

/// The Java and Kotlin sources of a checkout, as far as deciding whether a
/// class was compiled from one of them.
#[derive(Default)]
struct Sources {
    /// [`source_stems`] of every source path.
    stems: BTreeSet<String>,
    /// Kotlin file name to the internal package names its files declare.
    kotlin_packages: BTreeMap<String, BTreeSet<String>>,
}

/// The internal package name a Kotlin file's `package` header declares,
/// empty for the default package.
fn kotlin_package(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("package "))
        .and_then(|rest| rest.split([';', ' ', '\t']).next())
        .map(|name| name.replace('.', "/"))
        .unwrap_or_default()
}

/// A Kotlin source or script, both of which the Kotlin adapter reads.
fn is_kotlin(path: &str) -> bool {
    path.ends_with(".kt") || path.ends_with(".kts")
}

fn sources(paths: &[String], read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>) -> Sources {
    let mut kotlin_packages: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for path in paths.iter().filter(|path| is_kotlin(path)) {
        let Ok(bytes) = read(path) else {
            continue;
        };
        let name = path.rsplit('/').next().unwrap_or(path);
        kotlin_packages
            .entry(name.to_string())
            .or_default()
            .insert(kotlin_package(&String::from_utf8_lossy(&bytes)));
    }
    Sources {
        stems: source_stems(paths),
        kotlin_packages,
    }
}

/// Whether the checkout holds the source of `class`: a `.java`, `.kt` or
/// `.kts` file at its outer class's package path or for its `…Kt` file facade,
/// the file its `SourceFile` attribute names at its package path, or a
/// Kotlin file of that name declaring its package (Kotlin files need not
/// sit at their package path, nor be named for their classes).
fn has_source(sources: &Sources, class: &ClassFile) -> bool {
    let internal = class.this_class.as_str();
    let outer = internal.split('$').next().unwrap_or(internal);
    if sources.stems.contains(outer)
        || outer
            .strip_suffix("Kt")
            .is_some_and(|facade| sources.stems.contains(facade))
    {
        return true;
    }
    let Some(file) = class.source_file.as_deref() else {
        return false;
    };
    let package = package_of(internal);
    let stem = file
        .strip_suffix(".java")
        .or_else(|| file.strip_suffix(".kt"))
        .or_else(|| file.strip_suffix(".kts"));
    let at_package = stem.is_some_and(|stem| {
        sources.stems.contains(&if package.is_empty() {
            stem.to_string()
        } else {
            format!("{package}/{stem}")
        })
    });
    at_package
        || sources
            .kotlin_packages
            .get(file)
            .is_some_and(|packages| packages.contains(package))
}

/// Read the compiled classes of the checkout at `root`: every `.class` file
/// the ordinary walk selects (`.gitignore` honored; hidden, dependency and
/// build-output directories not entered) whose class has no Java or Kotlin
/// source in the same walk. A repository built from its sources is read by
/// the source adapters; this reads the classes it ships without them. A file
/// that is not a readable class file is still read, so it becomes a Gap.
pub fn extract_dir(
    root: &std::path::Path,
    id: &SourceId<'_>,
    on_file: &mut dyn FnMut(&str),
) -> std::io::Result<Extraction> {
    let (classes, source_paths) = walk(root)?;
    let read = |rel: &str| std::fs::read(root.join(rel));
    let read_class = |rel: &str| read_class_file(&root.join(rel));
    extract_selected(id, &classes, &source_paths, &read, &read_class, on_file)
}

/// [`extract_dir`] over a capture: `classes` are the `.class` paths and
/// `source_paths` the Java and Kotlin paths it retained, and `read` returns
/// their retained bytes, not the checkout's. Which classes have source is
/// decided by the same capture the source adapters read, so an edit to the
/// checkout after capture neither drops nor duplicates a class.
pub fn extract_dir_from(
    id: &SourceId<'_>,
    classes: &[String],
    source_paths: &[String],
    read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
    on_file: &mut dyn FnMut(&str),
) -> std::io::Result<Extraction> {
    let read_class = |rel: &str| read(rel).map(Content::of);
    extract_selected(id, classes, source_paths, read, &read_class, on_file)
}

/// The `.class` files the ordinary walk of `root` selects, sorted: the
/// paths [`extract_dir`] reads, for a capture to retain.
pub fn class_paths(root: &std::path::Path) -> std::io::Result<Vec<String>> {
    Ok(walk(root)?.0)
}

/// The walk's `.class` paths and its Java and Kotlin source paths, sorted.
fn walk(root: &std::path::Path) -> std::io::Result<(Vec<String>, Vec<String>)> {
    let walked = source_walk::files(root, &skipped_dir, source_walk::Gitignores::Honor)?;
    let (mut classes, mut source_paths): (Vec<_>, Vec<_>) = walked
        .into_iter()
        .map(|file| file.rel)
        .filter(|rel| rel.ends_with(".class") || rel.ends_with(".java") || is_kotlin(rel))
        .partition(|rel| rel.ends_with(".class"));
    classes.sort();
    source_paths.sort();
    Ok((classes, source_paths))
}

fn extract_selected(
    id: &SourceId<'_>,
    classes: &[String],
    source_paths: &[String],
    read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
    read_class: &dyn Fn(&str) -> std::io::Result<Content>,
    on_file: &mut dyn FnMut(&str),
) -> std::io::Result<Extraction> {
    let sources = sources(source_paths, read);
    let mut files = Vec::new();
    let mut too_large = Vec::new();
    for rel in classes {
        on_file(rel);
        match read_class(rel)? {
            Content::Bytes(bytes) => {
                let covered =
                    class_file::parse(&bytes).is_ok_and(|class| has_source(&sources, &class));
                if !covered {
                    files.push((rel.clone(), bytes));
                }
            }
            Content::TooLarge { digest, len } => too_large.push((rel.clone(), digest, len)),
        }
    }
    Ok(extract_with(id, &files, &too_large))
}

#[cfg(test)]
mod tests;
