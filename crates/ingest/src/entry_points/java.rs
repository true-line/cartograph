//! Java programs (#2020): what starts work in a Java system, read with the
//! Java grammar and proven by the file's imports, never by a name alone.
//!
//! - a `public static void main(String[] args)` (or `String... args`,
//!   `String args[]`) is a process start. It is an HTTP server when it runs
//!   `SpringApplication` (`SpringApplication.run(…)`,
//!   `new SpringApplication(…)` or `new SpringApplicationBuilder(…)`, the
//!   class imported) and the
//!   repository declares Spring web endpoints (a file imports
//!   `org.springframework.web.bind.annotation`); a command under an
//!   operator directory; otherwise a process;
//! - a method annotated `@KafkaListener`, `@RabbitListener`, `@JmsListener`,
//!   `@SqsListener` or `@StreamListener` is a queue consumer, with the
//!   topics, queues or destinations its annotation names as literals;
//! - a method annotated `@Scheduled` is a scheduled job, with the schedule
//!   its annotation states as written;
//! - a class implementing `CommandLineRunner` or `ApplicationRunner` (its
//!   `run` method), and a `@Bean` method returning one, is a startup task.
//!
//! Each enters the method's Symbol when the Java adapter emitted exactly one
//! with that name, else the file. Test sources (`src/test/`, `src/it/`) and
//! test, example and end-to-end trees start nothing.

use super::{Kind, OPERATOR_DIRS, Start, Symbols, Target};
use core_prov::ConfidenceTier;
use std::collections::BTreeSet;
use tree_sitter::{Node as TsNode, Parser};

/// Directories whose programs are not part of the running system.
/// (`src/it/` is matched as a source root only: `it` is also a package.)
const NOT_SYSTEM_DIRS: &[&str] = &["e2e", "testdata", "examples", "example", "test", "tests"];

/// Listener annotations and the packages that declare them.
const LISTENERS: &[(&str, &[&str])] = &[
    ("KafkaListener", &["org.springframework.kafka.annotation"]),
    (
        "RabbitListener",
        &["org.springframework.amqp.rabbit.annotation"],
    ),
    ("JmsListener", &["org.springframework.jms.annotation"]),
    (
        "SqsListener",
        &[
            "io.awspring.cloud.sqs.annotation",
            "io.awspring.cloud.messaging.listener.annotation",
        ],
    ),
    (
        "StreamListener",
        &["org.springframework.cloud.stream.annotation"],
    ),
];

/// Annotation arguments that name what a listener consumes.
const LISTENER_SOURCES: &[&str] = &["topics", "queues", "destination", "value"];

const SCHEDULED: (&str, &[&str]) = ("Scheduled", &["org.springframework.scheduling.annotation"]);
const BEAN: (&str, &[&str]) = ("Bean", &["org.springframework.context.annotation"]);
const RUNNERS: &[&str] = &["CommandLineRunner", "ApplicationRunner"];
const BOOT: &[&str] = &["org.springframework.boot"];

/// Whether `rel` is a Java source this reader considers.
pub(super) fn candidate(rel: &str) -> bool {
    rel.ends_with(".java")
        && !rel.contains("vendor/")
        && !rel.starts_with("src/test/")
        && !rel.contains("/src/test/")
        && !rel.starts_with("src/it/")
        && !rel.contains("/src/it/")
        && !rel
            .split('/')
            .rev()
            .skip(1)
            .any(|dir| NOT_SYSTEM_DIRS.contains(&dir))
}

/// The starts every Java source in `files` declares.
pub(super) fn starts(repo: &str, files: &[(String, String)], symbols: &Symbols) -> Vec<Start> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .expect("tree-sitter-java grammar");
    let mut web: Option<(String, (u64, u64))> = None;
    let mut found: Vec<Found> = Vec::new();
    for (rel, text) in files {
        let Some(tree) = parser.parse(text, None) else {
            continue;
        };
        let cx = Cx {
            repo,
            rel,
            text,
            symbols,
            imports: imports(text, tree.root_node()),
        };
        if web.is_none()
            && let Some(span) = cx
                .imports
                .iter()
                .find(|i| {
                    i.path
                        .starts_with("org.springframework.web.bind.annotation")
                })
                .map(|i| i.span)
        {
            web = Some((rel.clone(), span));
        }
        let mut walk = tree.root_node().walk();
        for top in tree.root_node().named_children(&mut walk) {
            cx.type_declaration(top, &[], &mut found);
        }
    }
    // Overloads of one entry method are told apart by their signature.
    let mut seen: std::collections::BTreeMap<(String, String), usize> = Default::default();
    for found in &found {
        *seen
            .entry((found.start.declared_in.clone(), found.start.key.clone()))
            .or_default() += 1;
    }
    found
        .into_iter()
        .map(
            |Found {
                 mut start,
                 spring,
                 signature,
             }| {
                if seen[&(start.declared_in.clone(), start.key.clone())] > 1 {
                    start.key = format!("{}{signature}", start.key);
                    for (key, value) in &mut start.extra {
                        if *key == "handler" {
                            *value = start.key.clone().into();
                        }
                    }
                }
                if spring {
                    match &web {
                        Some(evidence) => {
                            start.kind = Kind {
                                kind: "http-server",
                                evidence: Some(evidence.clone()),
                            };
                        }
                        None => start.kind.kind = operator_or_process(&start.declared_in),
                    }
                }
                start
            },
        )
        .collect()
}

fn operator_or_process(rel: &str) -> &'static str {
    let dirs: Vec<&str> = rel.split('/').collect();
    if dirs[..dirs.len().saturating_sub(1)]
        .iter()
        .any(|dir| OPERATOR_DIRS.contains(dir))
    {
        "command"
    } else {
        "process"
    }
}

/// A start, whether it runs Spring Boot, and its method's signature.
struct Found {
    start: Start,
    spring: bool,
    signature: String,
}

#[derive(Debug)]
struct Import {
    /// `a.b.C` or, for a wildcard, `a.b`.
    path: String,
    wildcard: bool,
    span: (u64, u64),
}

fn imports(text: &str, root: TsNode) -> Vec<Import> {
    let mut walk = root.walk();
    root.named_children(&mut walk)
        .filter(|n| n.kind() == "import_declaration")
        .filter_map(|n| {
            let raw = &text[n.byte_range()];
            let body = raw
                .trim()
                .strip_prefix("import")?
                .trim()
                .trim_end_matches(';')
                .trim();
            let body = body.strip_prefix("static ").unwrap_or(body).trim();
            let (path, wildcard) = match body.strip_suffix(".*") {
                Some(base) => (base.to_string(), true),
                None => (body.to_string(), false),
            };
            Some(Import {
                path: path.split_whitespace().collect(),
                wildcard,
                span: span(n),
            })
        })
        .collect()
}

fn span(node: TsNode) -> (u64, u64) {
    (node.start_byte() as u64, node.end_byte() as u64)
}

struct Cx<'a> {
    repo: &'a str,
    rel: &'a str,
    text: &'a str,
    symbols: &'a Symbols<'a>,
    imports: Vec<Import>,
}

impl Cx<'_> {
    fn text(&self, node: TsNode) -> &str {
        &self.text[node.byte_range()]
    }

    /// Whether `spelled` names `simple` from one of `packages`: by its
    /// qualified spelling, a named import or a wildcard import.
    fn proves(&self, spelled: &str, simple: &str, packages: &[&str]) -> bool {
        if let Some((package, name)) = spelled.rsplit_once('.') {
            return name == simple && packages.contains(&package);
        }
        spelled == simple
            && self.imports.iter().any(|i| {
                if i.wildcard {
                    packages.contains(&i.path.as_str())
                } else {
                    i.path
                        .rsplit_once('.')
                        .is_some_and(|(p, n)| n == simple && packages.contains(&p))
                }
            })
    }

    /// The annotations in a declaration's modifiers: (spelled name, node).
    fn annotations<'t>(&self, declaration: TsNode<'t>) -> Vec<(String, TsNode<'t>)> {
        let mut walk = declaration.walk();
        let Some(modifiers) = declaration
            .children(&mut walk)
            .find(|c| c.kind() == "modifiers")
        else {
            return Vec::new();
        };
        let mut walk = modifiers.walk();
        modifiers
            .named_children(&mut walk)
            .filter(|c| matches!(c.kind(), "marker_annotation" | "annotation"))
            .filter_map(|a| {
                let name = a.child_by_field_name("name")?;
                Some((self.text(name).split_whitespace().collect(), a))
            })
            .collect()
    }

    fn modifier_words(&self, declaration: TsNode) -> BTreeSet<String> {
        let mut walk = declaration.walk();
        let Some(modifiers) = declaration
            .children(&mut walk)
            .find(|c| c.kind() == "modifiers")
        else {
            return BTreeSet::new();
        };
        let mut walk = modifiers.walk();
        modifiers
            .children(&mut walk)
            .filter(|c| !matches!(c.kind(), "marker_annotation" | "annotation"))
            .map(|c| self.text(c).to_string())
            .collect()
    }

    fn type_declaration(&self, node: TsNode, outer: &[String], out: &mut Vec<Found>) {
        if !matches!(
            node.kind(),
            "class_declaration" | "enum_declaration" | "record_declaration"
        ) {
            return;
        }
        let (Some(name), Some(body)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("body"),
        ) else {
            return;
        };
        let mut path = outer.to_vec();
        path.push(self.text(name).to_string());
        let class = path.join(".");
        let runner = self.runner_interface(node);
        let mut walk = body.walk();
        for member in body.named_children(&mut walk) {
            match member.kind() {
                "method_declaration" => self.method(member, &class, runner, out),
                "class_declaration" | "enum_declaration" | "record_declaration" => {
                    self.type_declaration(member, &path, out)
                }
                "enum_body_declarations" => {
                    let mut inner = member.walk();
                    for m in member.named_children(&mut inner) {
                        if m.kind() == "method_declaration" {
                            self.method(m, &class, runner, out);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// The runner interface a class implements, proven by import.
    fn runner_interface(&self, class: TsNode) -> Option<&'static str> {
        let interfaces = class.child_by_field_name("interfaces")?;
        let mut found = None;
        visit(interfaces, &mut |n| {
            if found.is_none() && matches!(n.kind(), "type_identifier" | "scoped_type_identifier") {
                let spelled: String = self.text(n).split_whitespace().collect();
                found = RUNNERS
                    .iter()
                    .copied()
                    .find(|r| self.proves(&spelled, r, BOOT));
            }
        });
        found
    }

    fn method(
        &self,
        method: TsNode,
        class: &str,
        runner: Option<&'static str>,
        out: &mut Vec<Found>,
    ) {
        let Some(name) = method.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name);
        let annotations = self.annotations(method);
        let declared = (method.start_byte() as u64, header_end(method));
        let key = format!("{class}.{name}");
        let params = self.param_types(method);
        let signature = format!("({})", params.join(","));
        let start = |how: String, kind: &'static str, extra, extra_spans| Start {
            declared_in: self.rel.to_string(),
            span: declared,
            key: key.clone(),
            how,
            command: None,
            kind: Kind {
                kind,
                evidence: None,
            },
            port: None,
            extra,
            extra_spans,
            target: self.target(class, name, &signature),
        };
        let found = |start: Start, spring: bool| Found {
            start,
            spring,
            signature: signature.clone(),
        };
        let handler = ("handler", serde_json::Value::from(key.clone()));
        if name == "main" && self.is_main(method) {
            let spring = self.runs_spring(method);
            let kind = operator_or_process(self.rel);
            out.push(found(
                start(
                    "public static void main".into(),
                    kind,
                    Vec::new(),
                    Vec::new(),
                ),
                spring,
            ));
            return;
        }
        for (spelled, annotation) in &annotations {
            if let Some((simple, _)) = LISTENERS
                .iter()
                .find(|(simple, packages)| self.proves(spelled, simple, packages))
            {
                let sources = self.listener_sources(*annotation);
                let mut extra = vec![handler.clone()];
                if !sources.is_empty() {
                    extra.push(("sources", serde_json::json!(sources)));
                }
                out.push(found(
                    start(
                        format!("@{simple}"),
                        "queue-consumer",
                        extra,
                        vec![(self.rel.to_string(), span(*annotation))],
                    ),
                    false,
                ));
                return;
            }
            if self.proves(spelled, SCHEDULED.0, SCHEDULED.1) {
                let schedule = annotation
                    .child_by_field_name("arguments")
                    .map(|args| {
                        let raw = self.text(args);
                        raw[1..raw.len().saturating_sub(1)]
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                let mut extra = vec![handler.clone()];
                if !schedule.is_empty() {
                    extra.push(("schedule", schedule.into()));
                }
                out.push(found(
                    start(
                        "@Scheduled".into(),
                        "scheduled-job",
                        extra,
                        vec![(self.rel.to_string(), span(*annotation))],
                    ),
                    false,
                ));
                return;
            }
        }
        // A `run` of a runner class, or a `@Bean` method that returns one.
        let returned = method
            .child_by_field_name("type")
            .map(|t| self.text(t).split_whitespace().collect::<String>());
        let bean_runner = annotations
            .iter()
            .any(|(spelled, _)| self.proves(spelled, BEAN.0, BEAN.1))
            .then_some(returned.as_deref())
            .flatten()
            .and_then(|ty| RUNNERS.iter().copied().find(|r| self.proves(ty, r, BOOT)));
        // The interface's own `run`, not an overload of it.
        let runs = |runner: &str| match runner {
            "CommandLineRunner" => {
                matches!(params.as_slice(), [p] if matches!(p.as_str(), "String..." | "String[]"))
            }
            _ => {
                matches!(params.as_slice(), [p] if p.rsplit('.').next() == Some("ApplicationArguments"))
            }
        };
        let how = match (runner, bean_runner) {
            (Some(runner), _) if name == "run" && runs(runner) => runner,
            (_, Some(runner)) => runner,
            _ => return,
        };
        out.push(found(
            start(how.into(), "startup-task", vec![handler], Vec::new()),
            false,
        ));
    }

    /// The method's parameter types as the Java adapter spells its Symbol
    /// signature: each type as written, a varargs type with `...`.
    fn param_types(&self, method: TsNode) -> Vec<String> {
        let Some(params) = method.child_by_field_name("parameters") else {
            return Vec::new();
        };
        let mut walk = params.walk();
        params
            .named_children(&mut walk)
            .filter_map(|param| match param.kind() {
                "formal_parameter" => param
                    .child_by_field_name("type")
                    .map(|ty| self.text(ty).to_string()),
                "spread_parameter" => {
                    let mut inner = param.walk();
                    param
                        .named_children(&mut inner)
                        .find(|child| {
                            !matches!(
                                child.kind(),
                                "modifiers"
                                    | "annotation"
                                    | "marker_annotation"
                                    | "variable_declarator"
                            )
                        })
                        .map(|ty| format!("{}...", self.text(ty)))
                }
                _ => None,
            })
            .collect()
    }

    /// `public static void main` with one `String[]`/`String...` parameter.
    fn is_main(&self, method: TsNode) -> bool {
        let words = self.modifier_words(method);
        if !words.contains("public") || !words.contains("static") {
            return false;
        }
        if method
            .child_by_field_name("type")
            .is_none_or(|t| t.kind() != "void_type")
        {
            return false;
        }
        let Some(params) = method.child_by_field_name("parameters") else {
            return false;
        };
        let mut walk = params.walk();
        let params: Vec<TsNode> = params
            .named_children(&mut walk)
            .filter(|p| matches!(p.kind(), "formal_parameter" | "spread_parameter"))
            .collect();
        let [param] = params.as_slice() else {
            return false;
        };
        let string = |ty: &str| matches!(ty, "String" | "java.lang.String");
        let text: String = self
            .text(*param)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let text = text.strip_prefix("final ").unwrap_or(&text);
        match param.kind() {
            "spread_parameter" => text
                .split_once("...")
                .is_some_and(|(ty, _)| string(ty.trim())),
            _ => {
                let Some(ty) = param.child_by_field_name("type") else {
                    return false;
                };
                let ty: String = self.text(ty).split_whitespace().collect();
                match ty.strip_suffix("[]") {
                    Some(element) => string(element),
                    // `String args[]`
                    None => string(&ty) && text.trim_end().ends_with("[]"),
                }
            }
        }
    }

    /// Whether the method body runs Spring Boot's application.
    fn runs_spring(&self, method: TsNode) -> bool {
        let Some(body) = method.child_by_field_name("body") else {
            return false;
        };
        let mut runs = false;
        visit(body, &mut |n| match n.kind() {
            "method_invocation" => {
                let (Some(object), Some(name)) = (
                    n.child_by_field_name("object"),
                    n.child_by_field_name("name"),
                ) else {
                    return;
                };
                let object: String = self.text(object).split_whitespace().collect();
                if self.text(name) == "run" && self.proves(&object, "SpringApplication", BOOT) {
                    runs = true;
                }
            }
            "object_creation_expression" => {
                if let Some(ty) = n.child_by_field_name("type") {
                    let ty: String = self.text(ty).split_whitespace().collect();
                    if self.proves(&ty, "SpringApplication", BOOT)
                        || self.proves(
                            &ty,
                            "SpringApplicationBuilder",
                            &["org.springframework.boot.builder"],
                        )
                    {
                        runs = true;
                    }
                }
            }
            _ => {}
        });
        runs
    }

    /// The literal strings a listener annotation names as its sources.
    fn listener_sources(&self, annotation: TsNode) -> Vec<String> {
        let Some(args) = annotation.child_by_field_name("arguments") else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut walk = args.walk();
        for arg in args.named_children(&mut walk) {
            let value = match arg.kind() {
                "element_value_pair" => {
                    let key = arg.child_by_field_name("key").map(|k| self.text(k));
                    if !key.is_some_and(|k| LISTENER_SOURCES.contains(&k)) {
                        continue;
                    }
                    arg.child_by_field_name("value")
                }
                "string_literal" | "element_value_array_initializer" => Some(arg),
                _ => None,
            };
            let Some(value) = value else { continue };
            visit(value, &mut |n| {
                if n.kind() == "string_literal" {
                    let raw = self.text(n);
                    if let Some(inner) = raw.strip_prefix('"').and_then(|r| r.strip_suffix('"'))
                        && !inner.contains(['\\', '$', '#'])
                    {
                        out.push(inner.to_string());
                    }
                }
            });
        }
        out
    }

    /// The method Symbol the adapter emitted for `class.name` with this
    /// signature, else the only one of that name; else the file.
    fn target(&self, class: &str, name: &str, signature: &str) -> Target {
        let exact = format!("sym:{}@{}#{class}.{name}{signature}", self.repo, self.rel);
        if self.symbols.contains(&exact) {
            return Target::File {
                rel: self.rel.to_string(),
                function: Some(format!("{class}.{name}{signature}")),
                confidence: ConfidenceTier::Confirmed,
                marker: None,
            };
        }
        let prefix = format!("sym:{}@{}#{class}.{name}(", self.repo, self.rel);
        let matches: Vec<&String> = self.symbols.with_prefix(&prefix);
        let function = match matches.as_slice() {
            [one] => one.split_once('#').map(|(_, f)| f.to_string()),
            _ => None,
        };
        Target::File {
            rel: self.rel.to_string(),
            function,
            confidence: ConfidenceTier::Confirmed,
            marker: None,
        }
    }
}

/// The end of a method's header: before its body, else the declaration.
fn header_end(method: TsNode) -> u64 {
    method
        .child_by_field_name("body")
        .map_or(method.end_byte(), |b| b.start_byte()) as u64
}

fn visit(node: TsNode, f: &mut dyn FnMut(TsNode)) {
    f(node);
    let mut walk = node.walk();
    for child in node.named_children(&mut walk) {
        visit(child, f);
    }
}
