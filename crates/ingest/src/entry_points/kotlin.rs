//! Kotlin programs (#2021, AC-1104): what starts work in a Kotlin system,
//! read with the Kotlin grammar and proven by the file's imports, never by
//! a name alone.
//!
//! - a top-level `fun main()` or `fun main(args: Array<String>)` (or
//!   `vararg args: String`), and a `@JvmStatic` one of that shape in an
//!   `object` or companion object, is a process start. It is an HTTP server
//!   when its body starts Ktor (`embeddedServer(…)` from
//!   `io.ktor.server.engine`, or an engine's `EngineMain`), or when it runs
//!   Spring Boot (`runApplication`, `SpringApplication` or
//!   `SpringApplicationBuilder`, imported from `org.springframework.boot`)
//!   and the repository declares Spring web endpoints (a Kotlin or Java file
//!   imports `org.springframework.web.bind.annotation`); a command under an
//!   operator directory; otherwise a process;
//! - a function annotated `@KafkaListener`, `@RabbitListener`,
//!   `@JmsListener`, `@SqsListener` or `@StreamListener` is a queue
//!   consumer, with the topics, queues or destinations its annotation names
//!   as literals;
//! - a function annotated `@Scheduled` is a scheduled job, with the schedule
//!   its annotation states as written;
//! - the `run` of a class implementing `CommandLineRunner` or
//!   `ApplicationRunner`, and a `@Bean` function declared to return one, is
//!   a startup task;
//! - the `doWork` of a class extending WorkManager's `Worker` or
//!   `CoroutineWorker`, the `createWork` of an `RxWorker` and the
//!   `startWork` of a `ListenableWorker` (imported from `androidx.work`) is
//!   a background job (AC-1110).
//!
//! Each enters the function's Symbol when the Kotlin adapter emitted it,
//! else the file. Test sources (`src/test/`, `src/it/`) and test, example
//! and end-to-end trees start nothing.

use super::{Kind, OPERATOR_DIRS, Start, Symbols, Target};
use core_prov::ConfidenceTier;
use tree_sitter::{Node as TsNode, Parser};

/// Directories whose programs are not part of the running system.
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
const BOOT_BUILDER: &[&str] = &["org.springframework.boot.builder"];
const KTOR_ENGINE: &[&str] = &["io.ktor.server.engine"];
const KTOR_ENGINE_MAINS: &[&str] = &[
    "io.ktor.server.netty",
    "io.ktor.server.cio",
    "io.ktor.server.jetty",
    "io.ktor.server.jetty.jakarta",
    "io.ktor.server.tomcat",
    "io.ktor.server.tomcat.jakarta",
];
const SPRING_WEB: &str = "org.springframework.web.bind.annotation";
/// WorkManager worker base classes and the function that does their work.
const WORKERS: &[(&str, &str, &[&str])] = &[
    ("Worker", "doWork", &["androidx.work"]),
    ("CoroutineWorker", "doWork", &["androidx.work"]),
    ("ListenableWorker", "startWork", &["androidx.work"]),
    (
        "RxWorker",
        "createWork",
        &["androidx.work", "androidx.work.rxjava3"],
    ),
];

/// Whether `rel` is a Kotlin source this reader considers.
pub(super) fn candidate(rel: &str) -> bool {
    rel.ends_with(".kt")
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

/// The first import of Spring web annotations among the Kotlin and Java
/// sources, as evidence that the repository serves Spring endpoints: an
/// import of the package or one of its members, read from the parsed
/// imports so a commented-out line or a longer package states nothing.
pub(super) fn spring_web(
    kotlin: &[(String, String)],
    java: &[(String, String)],
) -> Option<(String, (u64, u64))> {
    let first = |files: &[(String, String)], language: tree_sitter::Language, kind: &str| {
        let mut parser = Parser::new();
        parser.set_language(&language).expect("grammar");
        files.iter().find_map(|(rel, text)| {
            let tree = parser.parse(text, None)?;
            let root = tree.root_node();
            let mut walk = root.walk();
            root.named_children(&mut walk)
                .filter(|n| n.kind() == kind)
                .find(|n| imports_spring_web(&text[n.byte_range()]))
                .map(|n| (rel.clone(), span(n)))
        })
    };
    first(kotlin, tree_sitter_kotlin_ng::LANGUAGE.into(), "import").or_else(|| {
        first(
            java,
            tree_sitter_java::LANGUAGE.into(),
            "import_declaration",
        )
    })
}

/// Whether an import names `org.springframework.web.bind.annotation` or
/// one of its members.
fn imports_spring_web(import: &str) -> bool {
    let Some(body) = import.trim().strip_prefix("import") else {
        return false;
    };
    let body = body.trim().trim_end_matches(';');
    let body = body.strip_prefix("static ").unwrap_or(body);
    let body = body.split(" as ").next().unwrap_or(body);
    let path: String = body.split_whitespace().collect();
    let path = path.strip_suffix(".*").unwrap_or(&path);
    path == SPRING_WEB
        || path
            .strip_prefix(SPRING_WEB)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(|member| !member.is_empty() && !member.contains('.'))
}

/// The starts every Kotlin source in `files` declares. `web` is the
/// evidence that the repository serves Spring endpoints, if any.
pub(super) fn starts(
    repo: &str,
    files: &[(String, String)],
    web: Option<(String, (u64, u64))>,
    symbols: &Symbols,
) -> Vec<Start> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
        .expect("tree-sitter-kotlin grammar");
    let mut out = Vec::new();
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
            web: web.as_ref(),
        };
        let mut walk = tree.root_node().walk();
        for top in tree.root_node().named_children(&mut walk) {
            match top.kind() {
                "function_declaration" => cx.function_in(top, &[], Scope::TopLevel, None, &mut out),
                "class_declaration" | "object_declaration" => {
                    cx.type_declaration(top, &[], &mut out)
                }
                _ => {}
            }
        }
    }
    out
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

#[derive(Debug)]
struct Import {
    /// `a.b.C` or, for a wildcard, `a.b`.
    path: String,
    /// The name it binds (`C`, or an alias); `None` for a wildcard.
    binds: Option<String>,
}

fn imports(text: &str, root: TsNode) -> Vec<Import> {
    let mut walk = root.walk();
    root.named_children(&mut walk)
        .filter(|n| n.kind() == "import")
        .filter_map(|n| {
            let raw = text[n.byte_range()].replace(['\n', '\\'], " ");
            let body = raw
                .trim()
                .strip_prefix("import")?
                .trim()
                .trim_end_matches(';');
            let (body, alias) = match body.rsplit_once(" as ") {
                Some((path, alias)) => (path.trim(), Some(alias.trim().to_string())),
                None => (body.trim(), None),
            };
            let body: String = body.split_whitespace().collect();
            Some(match body.strip_suffix(".*") {
                Some(base) => Import {
                    path: base.to_string(),
                    binds: None,
                },
                None => {
                    Import {
                        binds: Some(alias.unwrap_or_else(|| {
                            body.rsplit('.').next().unwrap_or(&body).to_string()
                        })),
                        path: body,
                    }
                }
            })
        })
        .collect()
}

fn span(node: TsNode) -> (u64, u64) {
    (node.start_byte() as u64, node.end_byte() as u64)
}

fn named_children(node: TsNode<'_>) -> Vec<TsNode<'_>> {
    let mut walk = node.walk();
    node.named_children(&mut walk).collect()
}

/// Where a function is declared, for the `main` shapes the JVM starts.
#[derive(Clone, Copy, PartialEq)]
enum Scope {
    TopLevel,
    /// An `object` or companion object member.
    Object,
    Class,
}

struct Cx<'a> {
    repo: &'a str,
    rel: &'a str,
    text: &'a str,
    symbols: &'a Symbols<'a>,
    imports: Vec<Import>,
    web: Option<&'a (String, (u64, u64))>,
}

impl Cx<'_> {
    fn text(&self, node: TsNode) -> &str {
        &self.text[node.byte_range()]
    }

    /// Whether `spelled` names `simple` from one of `packages`: by its
    /// qualified spelling, a named (or aliased) import or a star import that
    /// no named import of that name overrides.
    fn proves(&self, spelled: &str, simple: &str, packages: &[&str]) -> bool {
        if let Some((package, name)) = spelled.rsplit_once('.') {
            return name == simple && packages.contains(&package);
        }
        if let Some(named) = self
            .imports
            .iter()
            .find(|i| i.binds.as_deref() == Some(spelled))
        {
            return named
                .path
                .rsplit_once('.')
                .is_some_and(|(p, n)| n == simple && packages.contains(&p));
        }
        spelled == simple
            && self
                .imports
                .iter()
                .any(|i| i.binds.is_none() && packages.contains(&i.path.as_str()))
    }

    /// The annotations on a declaration: (spelled name, node).
    fn annotations<'t>(&self, declaration: TsNode<'t>) -> Vec<(String, TsNode<'t>)> {
        let Some(modifiers) = named_children(declaration)
            .into_iter()
            .find(|c| c.kind() == "modifiers")
        else {
            return Vec::new();
        };
        named_children(modifiers)
            .into_iter()
            .filter(|c| c.kind() == "annotation")
            .filter_map(|annotation| {
                let head = named_children(annotation).into_iter().next()?;
                let ty = match head.kind() {
                    "user_type" => head,
                    "constructor_invocation" => named_children(head)
                        .into_iter()
                        .find(|c| c.kind() == "user_type")?,
                    _ => return None,
                };
                Some((self.text(ty).split_whitespace().collect(), annotation))
            })
            .collect()
    }

    fn type_declaration(&self, node: TsNode, outer: &[String], out: &mut Vec<Start>) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let mut path = outer.to_vec();
        path.push(self.text(name).to_string());
        let scope = if node.kind() == "object_declaration" {
            Scope::Object
        } else {
            Scope::Class
        };
        let runner = self.runner_interface(node);
        self.body(node, &path, scope, runner, out);
        if node.kind() == "class_declaration" {
            self.worker(node, &path, out);
        }
    }

    /// A WorkManager worker's work function, when the class extends a
    /// worker base class proven by import (AC-1110).
    fn worker(&self, class: TsNode, path: &[String], out: &mut Vec<Start>) {
        let Some(specifiers) = named_children(class)
            .into_iter()
            .find(|c| c.kind() == "delegation_specifiers")
        else {
            return;
        };
        let Some((base, work)) = named_children(specifiers)
            .into_iter()
            .find_map(|specifier| {
                let first = specifier.named_child(0)?;
                let ty = match first.kind() {
                    "constructor_invocation" => named_children(first)
                        .into_iter()
                        .find(|c| c.kind() == "user_type")?,
                    "user_type" => first,
                    _ => return None,
                };
                let spelled: String = self.text(ty).split_whitespace().collect();
                WORKERS
                    .iter()
                    .find(|(simple, _, packages)| self.proves(&spelled, simple, packages))
                    .map(|(simple, work, _)| (*simple, *work))
            })
        else {
            return;
        };
        let Some(body) = named_children(class)
            .into_iter()
            .find(|c| c.kind() == "class_body")
        else {
            return;
        };
        let Some(function) = named_children(body).into_iter().find(|member| {
            member.kind() == "function_declaration"
                && member
                    .child_by_field_name("name")
                    .is_some_and(|name| self.text(name) == work)
                && self.params(*member).is_empty()
        }) else {
            return;
        };
        let key = path
            .iter()
            .map(String::as_str)
            .chain([work])
            .collect::<Vec<_>>()
            .join(".");
        out.push(Start {
            declared_in: self.rel.to_string(),
            span: (function.start_byte() as u64, header_end(function)),
            key: key.clone(),
            how: base.to_string(),
            command: None,
            kind: Kind {
                kind: "background-job",
                evidence: None,
            },
            port: None,
            extra: vec![
                ("handler", serde_json::Value::from(key.clone())),
                ("component", serde_json::Value::from(path.join("."))),
            ],
            extra_spans: Vec::new(),
            target: self.target(&key),
        });
    }

    fn body(
        &self,
        node: TsNode,
        path: &[String],
        scope: Scope,
        runner: Option<&'static str>,
        out: &mut Vec<Start>,
    ) {
        let Some(body) = named_children(node)
            .into_iter()
            .find(|c| matches!(c.kind(), "class_body" | "enum_class_body"))
        else {
            return;
        };
        for member in named_children(body) {
            match member.kind() {
                "function_declaration" => {
                    self.function_in(member, path, scope, runner, out);
                }
                // A companion object is transparent in member names.
                "companion_object" => self.body(member, path, Scope::Object, None, out),
                "class_declaration" | "object_declaration" => {
                    self.type_declaration(member, path, out)
                }
                _ => {}
            }
        }
    }

    /// The runner interface a class implements, proven by import.
    fn runner_interface(&self, class: TsNode) -> Option<&'static str> {
        let specifiers = named_children(class)
            .into_iter()
            .find(|c| c.kind() == "delegation_specifiers")?;
        named_children(specifiers)
            .into_iter()
            .find_map(|specifier| {
                let ty = specifier
                    .named_child(0)
                    .filter(|first| first.kind() == "user_type")?;
                let spelled: String = self.text(ty).split_whitespace().collect();
                RUNNERS
                    .iter()
                    .copied()
                    .find(|r| self.proves(&spelled, r, BOOT))
            })
    }

    fn function_in(
        &self,
        function: TsNode,
        path: &[String],
        scope: Scope,
        runner: Option<&'static str>,
        out: &mut Vec<Start>,
    ) {
        let Some(name) = function.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name);
        // An extension function's receiver is not part of its Symbol name.
        let key = path
            .iter()
            .map(String::as_str)
            .chain([name])
            .collect::<Vec<_>>()
            .join(".");
        let annotations = self.annotations(function);
        let declared = (function.start_byte() as u64, header_end(function));
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
            target: self.target(&key),
        };
        let handler = ("handler", serde_json::Value::from(key.clone()));
        if name == "main" && !has_receiver(function) && self.is_main(function, scope, &annotations)
        {
            let mut kind = Kind {
                kind: operator_or_process(self.rel),
                evidence: None,
            };
            match self.runs(function) {
                Some(Runs::Ktor(at)) => {
                    kind = Kind {
                        kind: "http-server",
                        evidence: Some((self.rel.to_string(), at)),
                    }
                }
                Some(Runs::Spring) => {
                    if let Some(web) = self.web {
                        kind = Kind {
                            kind: "http-server",
                            evidence: Some(web.clone()),
                        };
                    }
                }
                None => {}
            }
            let mut main = start("fun main".into(), kind.kind, Vec::new(), Vec::new());
            main.kind = kind;
            out.push(main);
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
                out.push(start(
                    format!("@{simple}"),
                    "queue-consumer",
                    extra,
                    vec![(self.rel.to_string(), span(*annotation))],
                ));
                return;
            }
            if self.proves(spelled, SCHEDULED.0, SCHEDULED.1) {
                let schedule = self
                    .annotation_arguments(*annotation)
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
                out.push(start(
                    "@Scheduled".into(),
                    "scheduled-job",
                    extra,
                    vec![(self.rel.to_string(), span(*annotation))],
                ));
                return;
            }
        }
        let params = self.params(function);
        // A `run` of a runner class, or a `@Bean` function declared to
        // return one.
        let returned =
            return_type(function).map(|t| self.text(t).split_whitespace().collect::<String>());
        let bean_runner = annotations
            .iter()
            .any(|(spelled, _)| self.proves(spelled, BEAN.0, BEAN.1))
            .then_some(returned.as_deref())
            .flatten()
            .and_then(|ty| RUNNERS.iter().copied().find(|r| self.proves(ty, r, BOOT)));
        // The interface's own `run`, not an overload of it.
        let runs = |runner: &str| match runner {
            "CommandLineRunner" => matches!(params.as_slice(), [p] if p.vararg && p.ty == "String"),
            _ => {
                matches!(params.as_slice(), [p] if !p.vararg && p.ty.rsplit('.').next() == Some("ApplicationArguments"))
            }
        };
        let how = match (runner, bean_runner) {
            (Some(runner), _) if name == "run" && runs(runner) => runner,
            (_, Some(runner)) => runner,
            _ => return,
        };
        out.push(start(how.into(), "startup-task", vec![handler], Vec::new()));
    }

    /// The parameters of a function: each type as written (nullability
    /// dropped) and whether it is `vararg`.
    fn params(&self, function: TsNode) -> Vec<Param> {
        let Some(list) = named_children(function)
            .into_iter()
            .find(|c| c.kind() == "function_value_parameters")
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut vararg = false;
        for child in named_children(list) {
            match child.kind() {
                "parameter_modifiers" => {
                    vararg = self.text(child).split_whitespace().any(|w| w == "vararg");
                }
                "parameter" => {
                    let ty = named_children(child)
                        .into_iter()
                        .find(|c| matches!(c.kind(), "user_type" | "nullable_type"))
                        .map(|ty| {
                            self.text(ty)
                                .split_whitespace()
                                .collect::<String>()
                                .trim_end_matches('?')
                                .to_string()
                        })
                        .unwrap_or_default();
                    out.push(Param { ty, vararg });
                    vararg = false;
                }
                _ => {}
            }
        }
        out
    }

    /// The JVM `main` shapes: top level, no parameter or one
    /// `Array<String>` / `vararg String`; in an object, `@JvmStatic` with
    /// one such parameter.
    fn is_main(&self, function: TsNode, scope: Scope, annotations: &[(String, TsNode)]) -> bool {
        let params = self.params(function);
        let string = |ty: &str| matches!(ty, "String" | "kotlin.String");
        let args = match params.as_slice() {
            [p] if p.vararg => string(&p.ty),
            [p] => {
                p.ty.strip_prefix("Array<")
                    .and_then(|rest| rest.strip_suffix('>'))
                    .is_some_and(|element| string(element.trim_start_matches("out")))
            }
            _ => false,
        };
        match scope {
            Scope::TopLevel => params.is_empty() || args,
            Scope::Object => {
                args && annotations
                    .iter()
                    .any(|(spelled, _)| spelled == "JvmStatic" || spelled == "kotlin.jvm.JvmStatic")
            }
            Scope::Class => false,
        }
    }

    /// What a `main` body starts: Ktor (with the span that shows it) or
    /// Spring Boot.
    fn runs(&self, function: TsNode) -> Option<Runs> {
        let body = named_children(function)
            .into_iter()
            .find(|c| c.kind() == "function_body")?;
        let mut found = None;
        visit(body, &mut |n| {
            if found.is_some() || n.kind() != "identifier" {
                return;
            }
            let name = self.text(n);
            // Only a call starts a server: `embeddedServer(…)`,
            // `runApplication<App>(…)`, `SpringApplicationBuilder(…)`,
            // `SpringApplication.run(…)` or `EngineMain.main(…)`; a type or
            // callable reference does not.
            let called = called(n);
            let member = member_called(n).map(|member| self.text(member));
            if (called && self.proves(name, "embeddedServer", KTOR_ENGINE))
                || (member == Some("main") && self.proves(name, "EngineMain", KTOR_ENGINE_MAINS))
            {
                found = Some(Runs::Ktor(span(n)));
            } else if (called && self.proves(name, "runApplication", BOOT))
                || (member == Some("run") && self.proves(name, "SpringApplication", BOOT))
                || (called && self.proves(name, "SpringApplicationBuilder", BOOT_BUILDER))
            {
                found = Some(Runs::Spring);
            }
        });
        found
    }

    fn annotation_arguments<'t>(&self, annotation: TsNode<'t>) -> Option<TsNode<'t>> {
        let invocation = named_children(annotation)
            .into_iter()
            .find(|c| c.kind() == "constructor_invocation")?;
        named_children(invocation)
            .into_iter()
            .find(|c| c.kind() == "value_arguments")
    }

    /// The literal strings a listener annotation names as its sources.
    fn listener_sources(&self, annotation: TsNode) -> Vec<String> {
        let Some(args) = self.annotation_arguments(annotation) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for arg in named_children(args) {
            if arg.kind() != "value_argument" {
                continue;
            }
            let parts = named_children(arg);
            let value = match parts.as_slice() {
                [key, value]
                    if key.kind() == "identifier"
                        && LISTENER_SOURCES.contains(&self.text(*key)) =>
                {
                    *value
                }
                [_, _] => continue,
                [value] => *value,
                _ => continue,
            };
            visit(value, &mut |n| {
                if n.kind() == "string_literal" {
                    let raw = self.text(n);
                    if let Some(inner) = raw.strip_prefix('"').and_then(|r| r.strip_suffix('"'))
                        && !inner.contains(['\\', '$', '#', '"'])
                    {
                        out.push(inner.to_string());
                    }
                }
            });
        }
        out
    }

    /// The function Symbol the adapter emitted for `key`, else the file.
    fn target(&self, key: &str) -> Target {
        let exact = format!("sym:{}@{}#{key}", self.repo, self.rel);
        Target::File {
            rel: self.rel.to_string(),
            function: self.symbols.contains(&exact).then(|| key.to_string()),
            confidence: ConfidenceTier::Confirmed,
            marker: None,
        }
    }
}

struct Param {
    ty: String,
    vararg: bool,
}

enum Runs {
    Ktor((u64, u64)),
    Spring,
}

/// A function's declared return type: the type after its parameters.
fn return_type(function: TsNode<'_>) -> Option<TsNode<'_>> {
    let parts = named_children(function);
    let params = parts
        .iter()
        .position(|c| c.kind() == "function_value_parameters")?;
    parts[params + 1..]
        .iter()
        .copied()
        .take_while(|c| c.kind() != "function_body")
        .find(|c| matches!(c.kind(), "user_type" | "nullable_type"))
}

/// The end of a function's header: before its body, else the declaration.
fn header_end(function: TsNode) -> u64 {
    named_children(function)
        .into_iter()
        .find(|c| c.kind() == "function_body")
        .map_or(function.end_byte(), |b| b.start_byte()) as u64
}

/// Whether a function declares an extension receiver (`fun T.main()`):
/// its compiled method takes the receiver as a parameter.
fn has_receiver(function: TsNode) -> bool {
    let Some(name) = function.child_by_field_name("name") else {
        return false;
    };
    named_children(function).into_iter().any(|c| {
        c.end_byte() <= name.start_byte() && !matches!(c.kind(), "modifiers" | "type_parameters")
    })
}

/// Whether `name` is the callee of a call: `name(…)`, `name(…) { … }` or,
/// as the grammar reads a call with type arguments, `name<T>(…)`.
fn called(name: TsNode) -> bool {
    let Some(parent) = name.parent() else {
        return false;
    };
    if parent.kind() == "call_expression" {
        return parent.named_child(0) == Some(name);
    }
    parent.kind() == "binary_expression"
        && parent.named_child(0) == Some(name)
        && operator_is(parent, "<")
        && parent.parent().is_some_and(|outer| {
            outer.kind() == "binary_expression"
                && outer.named_child(0) == Some(parent)
                && operator_is(outer, ">")
                && outer
                    .named_child(1)
                    .is_some_and(|args| args.kind() == "parenthesized_expression")
        })
}

/// The member called on `receiver`: `m` in `receiver.m(…)`.
fn member_called(receiver: TsNode) -> Option<TsNode> {
    let navigation = receiver
        .parent()
        .filter(|p| p.kind() == "navigation_expression" && p.named_child(0) == Some(receiver))?;
    navigation
        .parent()
        .filter(|c| c.kind() == "call_expression" && c.named_child(0) == Some(navigation))?;
    navigation
        .named_child(1)
        .filter(|member| member.kind() == "identifier")
}

/// Whether a binary expression's operator token is `op`.
fn operator_is(node: TsNode, op: &str) -> bool {
    let mut walk = node.walk();
    node.children(&mut walk)
        .find(|c| !c.is_named())
        .is_some_and(|c| c.kind() == op)
}

fn visit(node: TsNode, f: &mut dyn FnMut(TsNode)) {
    f(node);
    let mut walk = node.walk();
    for child in node.named_children(&mut walk) {
        visit(child, f);
    }
}
