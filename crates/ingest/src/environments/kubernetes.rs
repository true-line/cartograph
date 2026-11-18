//! Kubernetes manifest documents (#1791, AC-0662, SPEC-14 increment 2).
//!
//! A YAML document with top-level scalar `apiVersion` and `kind` is a
//! manifest document. The pod-bearing kinds `Deployment`, `StatefulSet`,
//! `DaemonSet` and `Job` are read: `metadata.name`, `metadata.namespace`, a
//! literal `spec.replicas`, and for each entry of
//! `spec.template.spec.containers` and `initContainers` its name, image,
//! `containerPort` values, the names of its `env` entries and the names of
//! the ConfigMaps and Secrets its `envFrom` references.
//!
//! No `env[].value`, no `valueFrom` and no `data` or `stringData` is looked
//! at. Every stored scalar passes redaction.
//!
//! A `batch/*` `CronJob` is the schedule reader's. Every other kind is
//! present and not read, and coverage records it by kind.

use super::{Draft, Findings, Props, UNEVALUATED_YAML, find_kind, mapping_of, not_read, section};
use crate::pipelines::{Span, field, fields, items, span_of};
use crate::schedules::{first_mapping, scalar};
use std::collections::BTreeSet;
use tree_sitter::{Node as TsNode, Parser};

/// The manifest kinds read, each with the `kind` its `Workload` carries.
const READ_KINDS: &[(&str, &str)] = &[
    ("Deployment", "k8s-deployment"),
    ("StatefulSet", "k8s-statefulset"),
    ("DaemonSet", "k8s-daemonset"),
    ("Job", "k8s-job"),
];

/// One manifest document of a file.
#[derive(Debug)]
pub(super) struct Document {
    /// Its position among the YAML documents of the file, from 0.
    pub(super) index: usize,
    /// 1-based line of its first key.
    pub(super) line: u64,
    /// The whole document.
    pub(super) span: Span,
    /// Its `kind`, as written.
    pub(super) kind: String,
    /// A `batch/*` `CronJob`, which the schedule reader covers.
    pub(super) cronjob: bool,
    /// What was read, when the kind is one this reader reads.
    pub(super) read: Option<Read>,
}

/// What a document of a read kind states.
#[derive(Debug)]
pub(super) struct Read {
    /// `kind` of the `Workload`.
    pub(super) fact_kind: &'static str,
    /// `metadata.name`, when it is a plain name.
    pub(super) name: Option<String>,
    /// `namespace`, `replicas` and `containers`, each only where stated.
    pub(super) props: Props,
    /// False when the pod template has no `containers` key at all.
    pub(super) containers_stated: bool,
    /// Constructs that are not evaluated and properties not read.
    pub(super) findings: Findings,
}

/// True when both keys start a line, as they do in any block-style manifest.
/// Most YAML has neither and is not parsed.
pub(super) fn may_hold_manifest(text: &str) -> bool {
    let starts = |key: &str| text.lines().any(|line| line.starts_with(key));
    starts("apiVersion:") && starts("kind:")
}

/// A kind written as a plain name, which a message may quote.
pub(super) fn plain_kind(kind: &str) -> Option<&str> {
    (kind.len() <= 63
        && kind.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && kind.chars().all(|c| c.is_ascii_alphanumeric()))
    .then_some(kind)
}

/// The manifest documents of `text`; `None` when it is not valid YAML.
pub(super) fn documents(text: &str) -> Option<Vec<Document>> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_yaml::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(text, None)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let mut walk = root.walk();
    let documents: Vec<TsNode> = root
        .named_children(&mut walk)
        .filter(|node| node.kind() == "document")
        .collect();
    let mut out = Vec::new();
    for (index, document) in documents.into_iter().enumerate() {
        let Some(top) = first_mapping(document) else {
            continue;
        };
        let mut walk = top.walk();
        let mut api = None;
        let mut kind = None;
        for pair in top
            .named_children(&mut walk)
            .filter(|pair| pair.kind() == "block_mapping_pair")
        {
            let (Some(key), Some(value)) = (
                pair.child_by_field_name("key"),
                pair.child_by_field_name("value"),
            ) else {
                continue;
            };
            match scalar(text, key).as_deref() {
                Some("apiVersion") => api = scalar(text, value),
                Some("kind") => kind = scalar(text, value),
                _ => {}
            }
        }
        let (Some(api), Some(kind)) = (api, kind) else {
            continue;
        };
        let read = READ_KINDS
            .iter()
            .find(|(known, _)| *known == kind)
            .map(|(_, fact_kind)| read(text, document, top, fact_kind));
        out.push(Document {
            index,
            line: 1 + text[..top.start_byte()].matches('\n').count() as u64,
            span: span_of(document),
            cronjob: kind == "CronJob" && api.starts_with("batch/"),
            kind,
            read,
        });
    }
    Some(out)
}

/// A plain, unquoted non-negative integer.
fn integer(text: &str, node: TsNode) -> Option<u64> {
    let value = scalar(text, node)?;
    (text[node.byte_range()].trim() == value)
        .then(|| value.parse().ok())
        .flatten()
}

fn read(text: &str, document: TsNode, top: TsNode, fact_kind: &'static str) -> Read {
    let mut findings = Findings::default();
    for (kind, what) in UNEVALUATED_YAML {
        if let Some(found) = find_kind(document, kind) {
            findings.add(*what, span_of(found));
        }
    }
    let whole = span_of(document);
    let mut out = Read {
        fact_kind,
        name: None,
        props: Props::new(),
        containers_stated: false,
        findings,
    };
    let Ok(top) = fields(text, top) else {
        out.findings.add(
            "the document is not a mapping of distinct literal keys",
            whole,
        );
        return out;
    };
    let mut draft = Draft::default();
    if let Some(metadata) = section(text, &top, "metadata") {
        out.name = field(&metadata, "name")
            .and_then(|name| scalar(text, name.value?))
            .filter(|name| super::service_name(name));
        if let Some(namespace) = field(&metadata, "namespace") {
            match namespace.value.and_then(|value| scalar(text, value)) {
                Some(value) => draft.text("namespace", &value),
                None => out
                    .findings
                    .add(not_read("metadata.namespace"), namespace.span),
            }
        }
    }
    let spec = section(text, &top, "spec");
    if let Some(replicas) = spec
        .as_ref()
        .and_then(|spec| field(spec, "replicas")?.value)
        .and_then(|value| integer(text, value))
    {
        draft.set("replicas", replicas);
    }
    let pod = spec
        .and_then(|spec| section(text, &section(text, &spec, "template")?, "spec"))
        .unwrap_or_default();
    // In the order the file writes the two lists.
    let mut containers = Vec::new();
    let mut readable = false;
    for list in &pod {
        let init = match list.name.as_str() {
            "containers" => false,
            "initContainers" => true,
            _ => continue,
        };
        out.containers_stated |= !init;
        let entries = list.value.and_then(items).and_then(|entries| {
            entries
                .into_iter()
                .map(|entry| container(text, entry, init, &mut out.findings))
                .collect::<Option<Vec<serde_json::Value>>>()
        });
        match entries {
            Some(entries) => {
                readable = true;
                containers.extend(entries);
            }
            None => out.findings.add(
                format!("`{}` is not a sequence of mappings", list.name),
                list.span,
            ),
        }
    }
    if readable {
        draft.set("containers", containers);
    }
    out.props = draft.finish();
    out
}

/// One container: only what its entry states. `None` when the entry is not a
/// mapping of distinct literal keys.
fn container(
    text: &str,
    entry: TsNode,
    init: bool,
    findings: &mut Findings,
) -> Option<serde_json::Value> {
    let body = fields(text, mapping_of(entry)?).ok()?;
    let mut draft = Draft::default();
    if init {
        draft.set("init", true);
    }
    for key in ["name", "image"] {
        if let Some(found) = field(&body, key) {
            match found.value.and_then(|value| scalar(text, value)) {
                Some(value) => draft.text(key, &value),
                None => findings.add(not_read(key), found.span),
            }
        }
    }
    // Each list entry is a mapping; `read` gives what one entry names.
    let mut list = |key: &'static str,
                    stored: &'static str,
                    sorted: bool,
                    read: &dyn Fn(TsNode) -> Option<String>| {
        let Some(found) = field(&body, key) else {
            return;
        };
        let names = found.value.and_then(items).and_then(|entries| {
            entries
                .into_iter()
                .map(read)
                .collect::<Option<Vec<String>>>()
        });
        match names {
            Some(names) if sorted => {
                draft.list(stored, names.into_iter().collect::<BTreeSet<String>>());
            }
            Some(names) => draft.list(stored, names),
            None => findings.add(not_read(key), found.key_span),
        }
    };
    list("ports", "ports", false, &|entry| {
        named(text, entry, "containerPort")
    });
    // The name of each entry; `value` and `valueFrom` are never looked at.
    list("env", "environment_keys", true, &|entry| {
        named(text, entry, "name")
    });
    list("envFrom", "env_from", false, &|entry| {
        let entry = fields(text, mapping_of(entry)?).ok()?;
        [("configMapRef", "ConfigMap"), ("secretRef", "Secret")]
            .into_iter()
            .find_map(|(key, what)| {
                let reference = section(text, &entry, key)?;
                let name = scalar(text, field(&reference, "name")?.value?)?;
                Some(format!("{what}/{name}"))
            })
    });
    Some(serde_json::Value::Object(draft.finish()))
}

/// The scalar `key` of a list entry that is a mapping.
fn named(text: &str, entry: TsNode, key: &str) -> Option<String> {
    let entry = fields(text, mapping_of(entry)?).ok()?;
    scalar(text, field(&entry, key)?.value?)
}
