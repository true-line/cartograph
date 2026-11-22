//! Per-environment settings files (#2167, ADR-0070 decision 10, SPEC-14
//! increment 3).
//!
//! - **Spring Boot profiles.** Spring Boot loads `application.{yml,yaml,
//!   properties}` and, for each active profile `P`, `application-P.*` from the
//!   same directory. A document of a base file that states
//!   `spring.config.activate.on-profile: P` (or the older document-level
//!   `spring.profiles: P`) is an `Environment` named `P`, and so is a
//!   profile-specific file `application-P.*` whose profile a base file of the
//!   same directory declares (`spring.profiles.active`, `.include` or a
//!   `spring.profiles.group` entry). Each carries the key paths it sets, never
//!   a value, and cites only the span that states the profile: a settings
//!   file commonly holds credentials.
//! - A profile-specific file whose profile no base file declares is named
//!   only by its file name: a name-only Gap. So is a settings module or
//!   configuration file named like an environment
//!   (`settings/production.py`, `config/staging.yml`,
//!   `config/environments/production.rb`).
//! - A profile expression (`prod & cloud`, `!dev`, a list) is a Gap; nothing
//!   is evaluated.

use super::{Findings, TEMPLATE_WORDS};
use crate::pipelines::{Span, fields, items, sanitize, span_of};
use crate::schedules::scalar;
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::{Node as TsNode, Parser};

/// How deep a settings mapping is followed for key paths.
const MAX_KEY_DEPTH: usize = 8;

/// How many key paths one settings document contributes.
const MAX_KEYS: usize = 512;

/// File-name stems that conventionally name an environment. Used only to
/// raise a name-only Gap; never to name an `Environment`.
const ENVIRONMENT_WORDS: &[&str] = &[
    "canary",
    "ci",
    "demo",
    "dev",
    "development",
    "integration",
    "local",
    "perf",
    "preprod",
    "preproduction",
    "prod",
    "production",
    "qa",
    "sandbox",
    "stage",
    "staging",
    "test",
    "testing",
    "uat",
];

/// The key whose value activates a document for a profile.
const ON_PROFILE: &str = "spring.config.activate.on-profile";

/// The pre-2.4 document-level activation key.
const LEGACY_PROFILES: &str = "spring.profiles";

/// Keys whose values name profiles that are declared to exist.
const DECLARING: &[&str] = &["spring.profiles.active", "spring.profiles.include"];

/// The prefix of profile groups: `spring.profiles.group.<name>`.
const GROUP: &str = "spring.profiles.group.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    Yaml,
    Properties,
}

/// A Spring Boot configuration file name: `None` for any other file,
/// `Some((None, _))` for a base file, `Some((Some(profile), _))` for a
/// profile-specific one.
pub(super) fn spring_file(name: &str) -> Option<(Option<&str>, Format)> {
    let (stem, format) = if let Some(stem) = name.strip_suffix(".properties") {
        (stem, Format::Properties)
    } else {
        let stem = name
            .strip_suffix(".yml")
            .or_else(|| name.strip_suffix(".yaml"))?;
        (stem, Format::Yaml)
    };
    if stem == "application" {
        return Some((None, format));
    }
    let profile = stem.strip_prefix("application-")?;
    (!profile.is_empty()).then_some((Some(profile), format))
}

/// True for a profile name this reader records: a plain name, not an
/// expression or a list.
fn plain_profile(profile: &str) -> bool {
    !profile.is_empty()
        && profile
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// True for a settings module or configuration file whose name alone names
/// an environment: `settings/production.py`, `config/staging.yml`,
/// `config/environments/production.rb`.
pub(super) fn names_an_environment(rel: &str) -> bool {
    let mut parts = rel.rsplit('/');
    let Some(name) = parts.next() else {
        return false;
    };
    let dir = parts.next().unwrap_or_default();
    let parent = parts.next().unwrap_or_default();
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    let in_place = match dir {
        "settings" => extension == "py",
        "config" => matches!(extension, "yml" | "yaml" | "json" | "toml"),
        "environments" => {
            parent == "config" && matches!(extension, "rb" | "js" | "ts" | "yml" | "yaml" | "json")
        }
        _ => false,
    };
    in_place && ENVIRONMENT_WORDS.contains(&stem.to_ascii_lowercase().as_str())
}

/// True for a profile-specific file whose profile is not a template word.
pub(super) fn profile_names_an_environment(profile: &str) -> bool {
    profile
        .split(['.', '-', '_'])
        .filter(|part| !part.is_empty())
        .any(|part| !TEMPLATE_WORDS.contains(&part.to_ascii_lowercase().as_str()))
}

/// One document of a Spring configuration file.
#[derive(Debug, Default)]
pub(super) struct Document {
    /// The profile the document is activated for, with the span of the
    /// statement.
    pub(super) on_profile: Option<(String, Span)>,
    /// Profiles the document declares exist (`active`, `include`, groups).
    pub(super) declares: Vec<(String, Span)>,
    /// Key paths the document sets, activation keys excluded.
    pub(super) keys: BTreeSet<String>,
    /// Key paths left out beyond [`MAX_KEYS`].
    pub(super) omitted: usize,
    pub(super) findings: Findings,
}

/// The documents of a Spring configuration file, or why none is read.
pub(super) fn spring(text: &str, format: Format) -> Result<Vec<Document>, (&'static str, Span)> {
    match format {
        Format::Yaml => yaml_documents(text),
        Format::Properties => Ok(properties_documents(text)),
    }
}

fn yaml_documents(text: &str) -> Result<Vec<Document>, (&'static str, Span)> {
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
    let mut documents = Vec::new();
    let mut streams = vec![root];
    while let Some(node) = streams.pop() {
        let mut walk = node.walk();
        let children: Vec<TsNode> = node.named_children(&mut walk).collect();
        for child in children.into_iter().rev() {
            match child.kind() {
                "stream" => streams.push(child),
                "document" => documents.push(child),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for document in documents {
        let mut doc = Document::default();
        if let Some(top) = crate::schedules::first_mapping(document) {
            let mut keys = BTreeSet::new();
            walk_yaml(text, top, "", 0, &mut doc, &mut keys);
            finish_keys(&mut doc, keys);
        }
        out.push(doc);
    }
    Ok(out)
}

fn walk_yaml(
    text: &str,
    node: TsNode,
    prefix: &str,
    depth: usize,
    doc: &mut Document,
    keys: &mut BTreeSet<String>,
) {
    let Ok(entries) = fields(text, node) else {
        doc.findings.add(
            "a mapping in the file is not of distinct literal keys",
            span_of(node),
        );
        return;
    };
    for entry in entries {
        let key = safe_key(&entry.name);
        let path = if prefix.is_empty() {
            key
        } else {
            format!("{prefix}.{key}")
        };
        let Some(value) = entry.value else {
            keys.insert(path);
            continue;
        };
        // `spring.profiles` is an activation unless it is the mapping of
        // `active`, `include` and `group` beneath it.
        if path == ON_PROFILE || path == LEGACY_PROFILES && super::mapping_of(value).is_none() {
            match scalar(text, value) {
                Some(profile) => activation(doc, &profile, entry.span),
                None if items(value).is_some() => doc.findings.add(
                    "the document is activated by a profile expression or list, which is not \
                     evaluated",
                    entry.span,
                ),
                None => doc
                    .findings
                    .add("a profile activation is not a plain scalar", entry.span),
            }
            continue;
        }
        if DECLARING.contains(&path.as_str()) || path.starts_with(GROUP) {
            if let Some(group) = path.strip_prefix(GROUP) {
                declare(doc, group, entry.key_span);
            }
            match items(value) {
                Some(list) => {
                    for item in list {
                        match scalar(text, item) {
                            Some(names) => declare_list(doc, &names, span_of(item)),
                            None => doc
                                .findings
                                .add("a profile entry is not a plain scalar", span_of(item)),
                        }
                    }
                }
                None => match scalar(text, value) {
                    Some(names) => declare_list(doc, &names, entry.span),
                    None => doc
                        .findings
                        .add("a profile declaration is not a plain scalar", entry.span),
                },
            }
            keys.insert(path);
            continue;
        }
        if depth + 1 < MAX_KEY_DEPTH && super::mapping_of(value).is_some() {
            let before = keys.len();
            walk_yaml(text, value, &path, depth + 1, doc, keys);
            if keys.len() == before && !ON_PROFILE.starts_with(&format!("{path}.")) {
                keys.insert(path);
            }
        } else if items(value).is_some() {
            keys.insert(format!("{path}[]"));
        } else {
            keys.insert(path);
        }
    }
}

/// A key path with each dotted segment sanitized on its own: a long dotted
/// property name is not a token, but a segment can be.
fn safe_key(key: &str) -> String {
    key.split('.')
        .map(|segment| sanitize(segment).0)
        .collect::<Vec<_>>()
        .join(".")
}

fn activation(doc: &mut Document, profile: &str, span: Span) {
    let (safe, redacted) = sanitize(profile);
    if redacted > 0 || !plain_profile(&safe) {
        doc.findings.add(
            "the document is activated by a profile expression or list, which is not evaluated",
            span,
        );
    } else if doc.on_profile.is_none() {
        doc.on_profile = Some((safe, span));
    } else {
        doc.findings
            .add("the document states more than one profile activation", span);
    }
}

fn declare(doc: &mut Document, profile: &str, span: Span) {
    let (safe, redacted) = sanitize(profile);
    if redacted == 0 && plain_profile(&safe) {
        doc.declares.push((safe, span));
    } else {
        doc.findings.add(
            "a declared profile is not a plain name, so it is not recorded",
            span,
        );
    }
}

fn declare_list(doc: &mut Document, names: &str, span: Span) {
    for name in names
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        if name.contains("${") {
            doc.findings.add(
                "a profile declaration is a placeholder and not resolved",
                span,
            );
        } else {
            declare(doc, name, span);
        }
    }
}

fn finish_keys(doc: &mut Document, keys: BTreeSet<String>) {
    let total = keys.len();
    doc.keys = keys.into_iter().take(MAX_KEYS).collect();
    doc.omitted = total.saturating_sub(MAX_KEYS);
}

/// The documents of a `.properties` file: `#---` or `!---` separates them.
/// A key is everything before the first unescaped `=`, `:` or whitespace;
/// a value continued with a trailing backslash is stepped over.
fn properties_documents(text: &str) -> Vec<Document> {
    let mut out = Vec::new();
    let mut doc = Document::default();
    let mut keys = BTreeSet::new();
    let mut offset = 0u64;
    let mut continued = false;
    for raw in text.split_inclusive('\n') {
        let start = offset;
        offset += raw.len() as u64;
        let line = raw.trim_end_matches(['\n', '\r']);
        let was_continued = continued;
        continued = line.chars().rev().take_while(|c| *c == '\\').count() % 2 == 1;
        if was_continued {
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed == "#---" || trimmed == "!---" {
            finish_keys(&mut doc, std::mem::take(&mut keys));
            out.push(std::mem::take(&mut doc));
            continued = false;
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
            continued = false;
            continue;
        }
        let (key, value) = split_property(trimmed);
        let span = (start, start + line.len() as u64);
        let key = safe_key(&key);
        if key == ON_PROFILE || key == LEGACY_PROFILES {
            if continued {
                doc.findings
                    .add("a profile activation spans lines and is not read", span);
            } else {
                activation(&mut doc, value.trim(), span);
            }
            continue;
        }
        if DECLARING.contains(&key.as_str()) || key.starts_with(GROUP) {
            if let Some(group) = key.strip_prefix(GROUP) {
                declare(&mut doc, group, span);
            }
            if continued {
                doc.findings
                    .add("a profile declaration spans lines and is not read", span);
            } else {
                declare_list(&mut doc, value.trim(), span);
            }
        }
        keys.insert(key);
    }
    finish_keys(&mut doc, keys);
    out.push(doc);
    out
}

fn split_property(line: &str) -> (String, &str) {
    let mut key = String::new();
    let mut chars = line.char_indices();
    while let Some((index, c)) = chars.next() {
        match c {
            '\\' => {
                if let Some((_, escaped)) = chars.next() {
                    key.push(escaped);
                }
            }
            '=' | ':' => return (key, &line[index + 1..]),
            c if c.is_whitespace() => {
                let rest = line[index..].trim_start();
                let rest = rest.strip_prefix(['=', ':']).unwrap_or(rest);
                return (key, rest);
            }
            c => key.push(c),
        }
    }
    (key, "")
}

/// Every Spring configuration file of the walk, parsed once, with the
/// profiles each directory's base files declare.
#[derive(Default)]
pub(super) struct Index {
    pub(super) files: BTreeMap<String, Result<Vec<Document>, (&'static str, Span)>>,
    /// Directory → profile → the first base file and span that declare it.
    declared: BTreeMap<String, BTreeMap<String, (String, Span)>>,
}

fn dir_of(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(dir, _)| dir)
}

impl Index {
    pub(super) fn read(walked: &[source_walk::WalkedFile], src: &super::Source) -> Self {
        let mut index = Self::default();
        for file in walked {
            let Some((profile, format)) = spring_file(&file.name) else {
                continue;
            };
            let parsed = match src.text(&file.rel) {
                Ok(text) => spring(&text, format),
                Err(_) => Err((
                    "the file is not readable UTF-8 text",
                    (0, src.len_or_zero(&file.rel)),
                )),
            };
            if profile.is_none()
                && let Ok(documents) = &parsed
            {
                let declared = index
                    .declared
                    .entry(dir_of(&file.rel).to_string())
                    .or_default();
                for document in documents {
                    let named = document.declares.iter().chain(document.on_profile.iter());
                    for (profile, span) in named {
                        declared
                            .entry(profile.clone())
                            .or_insert_with(|| (file.rel.clone(), *span));
                    }
                }
            }
            index.files.insert(file.rel.clone(), parsed);
        }
        index
    }

    /// Where a base file of `rel`'s directory declares `profile`.
    pub(super) fn declared(&self, rel: &str, profile: &str) -> Option<&(String, Span)> {
        self.declared.get(dir_of(rel))?.get(profile)
    }
}
