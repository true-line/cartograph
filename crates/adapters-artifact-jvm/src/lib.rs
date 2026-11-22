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
    let hash = Sha256::digest(bytes);
    let mut out = String::with_capacity(71);
    out.push_str("sha256:");
    for byte in hash {
        out.push_str(&format!("{byte:02x}"));
    }
    out
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

/// A method's qualified name as the Java source adapter spells it:
/// `Owner.method(Type,Type)`, a constructor named for its class and listing
/// only the parameters its source declares.
fn method_name(owner: &str, class: &ClassFile, method: &str, descriptor: &str) -> Option<String> {
    let mut params = class_file::parameter_types(descriptor)?;
    let name = if method == "<init>" {
        let added = added_constructor_parameters(class, descriptor).min(params.len());
        params.drain(..added);
        owner.rsplit('.').next().unwrap_or(owner)
    } else {
        method
    };
    Some(format!("{owner}.{name}({})", params.join(",")))
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
}

/// Read `files` — `(repository-relative path, bytes)` pairs of `.class`
/// files — into graph facts. Output is ordered by path, so the same files
/// in any order give the same extraction.
pub fn extract(id: &SourceId<'_>, files: &[(String, Vec<u8>)]) -> Extraction {
    let mut out = Extraction::default();
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
                    reads.push(Read {
                        path,
                        digest,
                        class,
                        nested,
                    });
                }
            }
            Err(code) => {
                let gap = format!("gap:{}@{path}#jvm-class", id.repo);
                out.nodes.push(Node {
                    id: gap.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "path": path,
                        "reason": format!("unreadable JVM class file: {code}"),
                        "reason_code": code,
                        "attempted_tiers": ["T0"],
                        "prov": prov(
                            &digest,
                            (0, bytes.len() as u64),
                            ConfidenceTier::Gap,
                            &format!("Gap {gap}"),
                        ),
                    }),
                });
            }
        }
    }

    // Every emitted method of the read classes, by (owner, name, descriptor).
    let mut methods: BTreeMap<(String, String, String), String> = BTreeMap::new();
    for read in &reads {
        for method in &read.class.methods {
            if !emitted(method) {
                continue;
            }
            if let Some(name) =
                method_name(&read.nested, &read.class, &method.name, &method.descriptor)
            {
                methods.insert(
                    (
                        read.class.this_class.clone(),
                        method.name.clone(),
                        method.descriptor.clone(),
                    ),
                    symbol_id(id.repo, read.path, &name),
                );
            }
        }
    }

    for read in &reads {
        emit_class(id, read, &methods, &mut out);
    }
    out
}

fn emit_class(
    id: &SourceId<'_>,
    read: &Read<'_>,
    methods: &BTreeMap<(String, String, String), String>,
    out: &mut Extraction,
) {
    let Read {
        path,
        digest,
        class,
        nested,
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

    for method in class.methods.iter().filter(|method| emitted(method)) {
        let Some(name) = method_name(nested, class, &method.name, &method.descriptor) else {
            continue;
        };
        let source = symbol_id(id.repo, path, &name);
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
            let Some(dst) = methods.get(&key) else {
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

#[cfg(test)]
mod tests;
