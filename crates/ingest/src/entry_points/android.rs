//! Android app components (#2021, AC-1110): what the system starts in an
//! Android app, read from the components its `AndroidManifest.xml`
//! declares, never from a class name alone.
//!
//! - each `<activity>`, `<service>`, `<receiver>` and `<provider>` under
//!   `<application>` is a start of its kind: a screen a person opens (the
//!   app's launch when an intent filter states the `MAIN` action and the
//!   `LAUNCHER` category), a service started or bound, a receiver of the
//!   broadcasts its intent filters name, a content provider;
//! - its `android:name` resolves against the manifest's `package`, else the
//!   module's Gradle `namespace` (a leading `.` or a bare name is
//!   package-relative);
//! - it enters the component's lifecycle function (`onCreate`,
//!   `onStartCommand`/`onBind`/`onHandleIntent`, `onReceive`) when the
//!   language adapter emitted it, else the Kotlin or Java file declaring
//!   the class;
//! - a component whose class no repository source declares, or whose
//!   relative name has no package to resolve against, is a Gap naming it;
//!   one marked `tools:node="remove"` declares nothing.
//!
//! Manifests under test, `androidTest`, build output and example trees
//! start nothing. The manifest is read by a small tag scanner (comments,
//! processing instructions and CDATA skipped), so a commented-out
//! component declares nothing.

use super::{Kind, Start, Symbols, Target};
use core_prov::ConfidenceTier;
use std::collections::BTreeMap;
use tree_sitter::Parser;

/// Directories whose manifests are not part of the running system.
const NOT_SYSTEM_DIRS: &[&str] = &[
    "test",
    "tests",
    "androidTest",
    "testFixtures",
    "build",
    "e2e",
    "testdata",
    "examples",
    "example",
];

/// The component tags and the kind each starts.
const COMPONENTS: &[(&str, &str)] = &[
    ("activity", "android-activity"),
    ("service", "android-service"),
    ("receiver", "android-receiver"),
    ("provider", "android-provider"),
];

/// Whether `rel` is an Android manifest of the running system.
pub(super) fn candidate(rel: &str) -> bool {
    let mut parts = rel.split('/').rev();
    parts.next() == Some("AndroidManifest.xml") && !parts.any(|dir| NOT_SYSTEM_DIRS.contains(&dir))
}

/// The Gradle build files that may state a manifest's `namespace`: the
/// module's, for `<module>/src/<set>/AndroidManifest.xml` or a manifest at
/// the module root.
pub(super) fn build_files(rel: &str) -> Vec<String> {
    let dir = rel.rsplit_once('/').map_or("", |(dir, _)| dir);
    let module = match dir.rsplit_once("src/") {
        Some((module, set)) if !set.contains('/') => module.to_owned(),
        _ if dir.is_empty() => String::new(),
        _ => format!("{dir}/"),
    };
    vec![
        format!("{module}build.gradle.kts"),
        format!("{module}build.gradle"),
    ]
}

/// The `namespace` a Gradle build file states as a literal:
/// `namespace = "a.b"` or `namespace 'a.b'`.
pub(super) fn gradle_namespace(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("namespace")?;
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
        let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let value = rest[1..].split(quote).next()?;
        (!value.is_empty()
            && value
                .chars()
                .all(|c| c.is_alphanumeric() || c == '.' || c == '_'))
        .then(|| value.to_owned())
    })
}

/// One element the scanner read: its tag, attributes and span, and the
/// tags enclosing it (outermost first).
#[derive(Debug)]
struct Element {
    tag: String,
    attributes: BTreeMap<String, String>,
    span: (u64, u64),
    within: Vec<String>,
    /// The index of the component element it sits in, if any.
    component: Option<usize>,
    /// The index of the `<intent-filter>` it sits in, if any.
    filter: Option<usize>,
}

/// Scan an XML document's start tags. Comments, processing instructions,
/// declarations and CDATA are skipped; entity references in attribute
/// values are decoded for the five predefined entities.
fn scan(text: &str) -> Vec<Element> {
    let mut out: Vec<Element> = Vec::new();
    // (tag, index of the element in `out`)
    let mut stack: Vec<(String, usize)> = Vec::new();
    let bytes = text.as_bytes();
    let mut at = 0;
    while let Some(offset) = text[at..].find('<') {
        let start = at + offset;
        let rest = &text[start..];
        let skip = |end: &str| rest.find(end).map(|i| start + i + end.len());
        if rest.starts_with("<!--") {
            let Some(next) = skip("-->") else { break };
            at = next;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            let Some(next) = skip("]]>") else { break };
            at = next;
            continue;
        }
        if rest.starts_with("<?") {
            let Some(next) = skip("?>") else { break };
            at = next;
            continue;
        }
        if rest.starts_with("<!") {
            let Some(next) = skip(">") else { break };
            at = next;
            continue;
        }
        // The tag's end, outside quoted attribute values.
        let mut end = None;
        let mut quote: Option<u8> = None;
        for (i, b) in bytes[start + 1..].iter().enumerate() {
            match (quote, *b) {
                (Some(q), b) if b == q => quote = None,
                (Some(_), _) => {}
                (None, b'"' | b'\'') => quote = Some(*b),
                (None, b'>') => {
                    end = Some(start + 1 + i);
                    break;
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        let inner = &text[start + 1..end];
        at = end + 1;
        if let Some(closing) = inner.strip_prefix('/') {
            let tag = closing.trim();
            if let Some(position) = stack.iter().rposition(|(open, _)| open == tag) {
                stack.truncate(position);
            }
            continue;
        }
        let self_closing = inner.ends_with('/');
        let inner = inner.strip_suffix('/').unwrap_or(inner);
        let tag: String = inner.chars().take_while(|c| !c.is_whitespace()).collect();
        if tag.is_empty() {
            continue;
        }
        let attributes = attributes(&inner[tag.len()..]);
        let within: Vec<String> = stack.iter().map(|(tag, _)| tag.clone()).collect();
        let component = stack
            .iter()
            .rev()
            .map(|(_, index)| *index)
            .find(|index| COMPONENTS.iter().any(|(t, _)| *t == out[*index].tag));
        let filter = stack
            .iter()
            .rev()
            .map(|(_, index)| *index)
            .find(|index| out[*index].tag == "intent-filter");
        out.push(Element {
            tag: tag.clone(),
            attributes,
            span: (start as u64, (end + 1) as u64),
            within,
            component,
            filter,
        });
        if !self_closing {
            stack.push((tag, out.len() - 1));
        }
    }
    out
}

fn attributes(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        let Some(eq) = rest.find('=') else { break };
        let name = rest[..eq].trim().to_owned();
        let value = rest[eq + 1..].trim_start();
        let Some(quote) = value.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            break;
        };
        let Some(close) = value[1..].find(quote) else {
            break;
        };
        let raw = &value[1..1 + close];
        let decoded = raw
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&");
        if !name.is_empty() && !name.contains(char::is_whitespace) {
            out.insert(name, decoded);
        }
        rest = &value[close + 2..];
    }
    out
}

/// Where a repository class is declared: the source file and its language.
pub(super) struct Declared {
    rel: String,
    java: bool,
}

/// The top-level classes and objects the Kotlin and Java sources declare,
/// by fully qualified name.
pub(super) fn declared_classes(
    kotlin: &[(String, String)],
    java: &[(String, String)],
) -> BTreeMap<String, Declared> {
    let mut out = BTreeMap::new();
    let mut read = |files: &[(String, String)],
                    language: tree_sitter::Language,
                    package_kind: &str,
                    types: &[&str],
                    is_java: bool| {
        let mut parser = Parser::new();
        parser.set_language(&language).expect("grammar");
        for (rel, text) in files {
            let Some(tree) = parser.parse(text, None) else {
                continue;
            };
            let root = tree.root_node();
            let mut walk = root.walk();
            let mut package = String::new();
            let mut names = Vec::new();
            for top in root.named_children(&mut walk) {
                if top.kind() == package_kind {
                    let raw = &text[top.byte_range()];
                    let body = raw.trim().strip_prefix("package").unwrap_or(raw);
                    package = body
                        .trim()
                        .trim_end_matches(';')
                        .split_whitespace()
                        .collect();
                } else if types.contains(&top.kind())
                    && let Some(name) = top.child_by_field_name("name")
                {
                    names.push(text[name.byte_range()].to_owned());
                }
            }
            for name in names {
                let fqn = if package.is_empty() {
                    name
                } else {
                    format!("{package}.{name}")
                };
                out.entry(fqn).or_insert(Declared {
                    rel: rel.clone(),
                    java: is_java,
                });
            }
        }
    };
    read(
        kotlin,
        tree_sitter_kotlin_ng::LANGUAGE.into(),
        "package_header",
        &["class_declaration", "object_declaration"],
        false,
    );
    read(
        java,
        tree_sitter_java::LANGUAGE.into(),
        "package_declaration",
        &["class_declaration"],
        true,
    );
    out
}

/// The lifecycle functions a component of each kind is entered through,
/// in the order tried.
fn lifecycle(kind: &str) -> &'static [&'static str] {
    match kind {
        "android-service" => &["onStartCommand", "onBind", "onHandleIntent", "onCreate"],
        "android-receiver" => &["onReceive"],
        _ => &["onCreate"],
    }
}

/// The starts the manifest `rel` declares. `namespace` is the module's
/// Gradle namespace, used when the manifest states no `package`.
pub(super) fn starts(
    repo: &str,
    rel: &str,
    text: &str,
    namespace: Option<&str>,
    classes: &BTreeMap<String, Declared>,
    symbols: &Symbols,
) -> Vec<Start> {
    let elements = scan(text);
    let package = elements
        .iter()
        .find(|e| e.tag == "manifest" && e.within.is_empty())
        .and_then(|e| e.attributes.get("package").cloned())
        .filter(|p| !p.is_empty())
        .or_else(|| namespace.map(str::to_owned));
    let mut out = Vec::new();
    for (index, element) in elements.iter().enumerate() {
        let Some((_, kind)) = COMPONENTS.iter().find(|(tag, _)| *tag == element.tag) else {
            continue;
        };
        if element.within != ["manifest", "application"]
            || element.attributes.get("tools:node").map(String::as_str) == Some("remove")
        {
            continue;
        }
        let Some(name) = element
            .attributes
            .get("android:name")
            .filter(|n| !n.is_empty())
        else {
            continue;
        };
        let inside = |tag: &str| -> Vec<String> {
            elements
                .iter()
                .filter(|e| e.component == Some(index) && e.tag == tag)
                .filter_map(|e| e.attributes.get("android:name").cloned())
                .collect()
        };
        let actions = inside("action");
        // MAIN and LAUNCHER must be declared in the same intent filter.
        let declares = |filter: usize, tag: &str, name: &str| {
            elements.iter().any(|e| {
                e.filter == Some(filter)
                    && e.tag == tag
                    && e.attributes.get("android:name").map(String::as_str) == Some(name)
            })
        };
        let launcher = elements
            .iter()
            .enumerate()
            .filter(|(_, e)| e.component == Some(index) && e.tag == "intent-filter")
            .any(|(filter, _)| {
                declares(filter, "action", "android.intent.action.MAIN")
                    && declares(filter, "category", "android.intent.category.LAUNCHER")
            });
        let fqn = match (name.strip_prefix('.'), &package) {
            (Some(relative), Some(package)) => Some(format!("{package}.{relative}")),
            (Some(_), None) => None,
            (None, Some(package)) if !name.contains('.') => Some(format!("{package}.{name}")),
            (None, None) if !name.contains('.') => None,
            (None, _) => Some(name.clone()),
        };
        let mut extra: Vec<(&'static str, serde_json::Value)> = Vec::new();
        if let Some(fqn) = &fqn {
            extra.push(("component", fqn.clone().into()));
        } else {
            extra.push(("component", name.clone().into()));
        }
        if let Some(exported) = element.attributes.get("android:exported")
            && let Ok(exported) = exported.parse::<bool>()
        {
            extra.push(("exported", exported.into()));
        }
        if !actions.is_empty() {
            extra.push(("actions", serde_json::json!(actions)));
        }
        if launcher {
            extra.push(("launcher", true.into()));
        }
        let target = match fqn.as_deref().map(|fqn| (fqn, classes.get(fqn))) {
            None => Target::Gap(format!(
                "component {name} is relative and the manifest states no package and the module no namespace"
            )),
            Some((fqn, None)) => Target::Gap(format!(
                "component class {fqn} is not declared in this repository"
            )),
            Some((fqn, Some(declared))) => {
                let class = fqn.rsplit('.').next().unwrap_or(fqn);
                let function = lifecycle(kind).iter().find_map(|method| {
                    let exact = format!("sym:{repo}@{}#{class}.{method}", declared.rel);
                    if !declared.java {
                        return symbols
                            .contains(&exact)
                            .then(|| format!("{class}.{method}"));
                    }
                    let prefix = format!("{exact}(");
                    let found: Vec<&String> = symbols.with_prefix(&prefix);
                    match found.as_slice() {
                        [one] => one.split_once('#').map(|(_, f)| f.to_owned()),
                        _ => None,
                    }
                });
                Target::File {
                    rel: declared.rel.clone(),
                    function,
                    confidence: ConfidenceTier::Confirmed,
                    marker: None,
                }
            }
        };
        out.push(Start {
            declared_in: rel.to_owned(),
            span: element.span,
            key: format!("{}:{}", element.tag, fqn.as_deref().unwrap_or(name)),
            how: format!("<{}>", element.tag),
            command: None,
            kind: Kind {
                kind,
                evidence: None,
            },
            port: None,
            extra,
            extra_spans: Vec::new(),
            target,
        });
    }
    out
}
