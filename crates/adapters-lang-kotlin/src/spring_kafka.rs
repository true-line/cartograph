//! Spring Kafka event sites (#1366, AC-0468), as Java reads them (#1347).
//!
//! A producer is `<template>.send(…)` on a receiver proven to be
//! `org.springframework.kafka.core.KafkaTemplate` as [`Proof`] proves a
//! `RestTemplate`: a typed or constructed local, parameter or property (or
//! `this.x`) whose type, generic arguments aside, is spelled by the fully
//! qualified name or the simple name under its named import. Subclasses
//! (`ReplyingKafkaTemplate`, …) and `sendDefault` are not read, nor is a
//! `send` the file declares or imports as a function, nor a call with more
//! arguments than any `send` overload takes. The topic is the first of two
//! or more positional arguments; a one-argument `send(ProducerRecord)` or
//! `send(Message)` carries it at runtime. Named arguments and trailing
//! lambdas are not mapped.
//!
//! A consumer is a function annotated `@KafkaListener`, spelled by its fully
//! qualified name or under its named import, with one site per `topics`
//! element (`topics = ["a", "b"]`, `arrayOf(…)` or a single value). A
//! `topicPattern` or `topicPartitions` listener is one computed site.
//! Class-level listeners and their `@KafkaHandler` functions are not read.
//!
//! A topic is a literal only as a string literal with no template, `${…}`
//! property placeholder or `#{…}` expression; anything else is computed.

use adapters_fw::events::{ChannelRole, EventSite, IdentityExpr};
use tree_sitter::Node as TsNode;

use super::rest_template::{Proof, declared_functions, enclosing, named_children};
use super::{FileCx, Imports, pure_string_literal, spelling};

const KAFKA_TEMPLATE: &str = "org.springframework.kafka.core.KafkaTemplate";
const KAFKA_LISTENER: &str = "org.springframework.kafka.annotation.KafkaListener";
const KIND: &str = "kafka-topic";

/// A topic expression, classified.
fn topic(cx: &FileCx<'_>, node: TsNode<'_>) -> IdentityExpr {
    match pure_string_literal(cx, node) {
        Some(topic) if !topic.contains("#{") => IdentityExpr::Literal(topic),
        _ => IdentityExpr::Computed(cx.text(&node).to_string()),
    }
}

fn site(
    cx: &FileCx<'_>,
    node: TsNode<'_>,
    role: ChannelRole,
    identity: IdentityExpr,
    symbol: &str,
) -> EventSite {
    EventSite {
        kind: KIND.to_string(),
        role,
        identity,
        symbol: Some(symbol.to_string()),
        path: cx.path.to_string(),
        byte_start: node.start_byte() as u64,
        byte_end: node.end_byte() as u64,
        components: Default::default(),
        batch: None,
        registries: Vec::new(),
    }
}

/// The annotations on a declaration, as their spelled type and argument
/// list; the caller keeps those proven to be `KafkaListener`.
fn listeners<'t>(cx: &FileCx<'_>, declaration: TsNode<'t>) -> Vec<(String, Option<TsNode<'t>>)> {
    let mut found = Vec::new();
    for modifiers in named_children(declaration) {
        if modifiers.kind() != "modifiers" {
            continue;
        }
        for annotation in named_children(modifiers) {
            if annotation.kind() != "annotation" {
                continue;
            }
            for child in named_children(annotation) {
                match child.kind() {
                    "user_type" => found.push((spelling(cx.text(&child)), None)),
                    "constructor_invocation" => {
                        let parts = named_children(child);
                        if let Some(ty) = parts.iter().find(|part| part.kind() == "user_type") {
                            let arguments = parts
                                .iter()
                                .find(|part| part.kind() == "value_arguments")
                                .copied();
                            found.push((spelling(cx.text(ty)), arguments));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    // Every spelling goes to the caller's proof, which also resolves an
    // import alias (`import …KafkaListener as Listener`).
    found
}

/// The elements of a `topics` value: a collection literal or `arrayOf(…)`
/// call lists them, anything else is one.
fn elements<'t>(cx: &FileCx<'_>, value: TsNode<'t>) -> Vec<TsNode<'t>> {
    match value.kind() {
        "collection_literal" => named_children(value),
        "call_expression" => {
            let parts = named_children(value);
            // `arrayOf<String>(…)` carries its type arguments between.
            let arguments = match parts.as_slice() {
                [callee, arguments] | [callee, _, arguments]
                    if parts.len() == 2 || parts[1].kind() == "type_arguments" =>
                {
                    Some((*callee, *arguments))
                }
                _ => None,
            };
            match arguments {
                Some((callee, arguments))
                    if callee.kind() == "identifier"
                        && cx.text(&callee) == "arrayOf"
                        && arguments.kind() == "value_arguments" =>
                {
                    named_children(arguments)
                        .into_iter()
                        .filter_map(|argument| match named_children(argument).as_slice() {
                            [element] if argument.kind() == "value_argument" => Some(*element),
                            _ => None,
                        })
                        .collect()
                }
                _ => vec![value],
            }
        }
        _ => vec![value],
    }
}

/// The topics a `@KafkaListener` names: each `topics` element, or the
/// whole pattern or partition argument as one computed topic.
fn listener_topics<'t>(
    cx: &FileCx<'_>,
    arguments: Option<TsNode<'t>>,
) -> Vec<(TsNode<'t>, IdentityExpr)> {
    let mut topics = Vec::new();
    for argument in arguments.map(named_children).unwrap_or_default() {
        if argument.kind() != "value_argument" {
            continue;
        }
        let [key, value] = named_children(argument)[..] else {
            continue;
        };
        if key.kind() != "identifier" {
            continue;
        }
        match cx.text(&key) {
            "topics" => topics.extend(
                elements(cx, value)
                    .into_iter()
                    .map(|element| (element, topic(cx, element))),
            ),
            "topicPattern" | "topicPartitions" => topics.push((
                argument,
                IdentityExpr::Computed(cx.text(&argument).to_string()),
            )),
            _ => {}
        }
    }
    topics
}

/// Spring Kafka event sites in this file, owned by their innermost enclosing
/// function (producers) or their annotated function (consumers).
pub(crate) fn sites(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    functions: &[(TsNode<'_>, String)],
    imports: &Imports,
) -> Vec<EventSite> {
    let text = cx.text(&root);
    if !text.contains("KafkaTemplate") && !text.contains("KafkaListener") {
        return Vec::new();
    }
    let proof = Proof::new(cx, root, imports);
    let mut out = Vec::new();
    for (function, symbol) in functions {
        for (spelled, arguments) in listeners(cx, *function) {
            if !proof.names(&spelled, KAFKA_LISTENER) {
                continue;
            }
            for (node, identity) in listener_topics(cx, arguments) {
                out.push(site(cx, node, ChannelRole::Consumes, identity, symbol));
            }
        }
    }
    let declared = declared_functions(cx, root);
    let mut stack = vec![root];
    while let Some(call) = stack.pop() {
        stack.extend(named_children(call));
        if call.kind() != "call_expression" {
            continue;
        }
        let [callee, arguments] = named_children(call)[..] else {
            continue;
        };
        if callee.kind() != "navigation_expression" || arguments.kind() != "value_arguments" {
            continue;
        }
        // `send(…) { … }` parses as this call wrapped by the lambda's.
        if call.parent().is_some_and(|outer| {
            outer.kind() == "call_expression"
                && named_children(outer)
                    .iter()
                    .any(|part| part.kind() == "annotated_lambda")
        }) {
            continue;
        }
        let [receiver, name] = named_children(callee)[..] else {
            continue;
        };
        // A `send` the file declares or imports may be the function called.
        if name.kind() != "identifier"
            || cx.text(&name) != "send"
            || declared.contains("send")
            || imports.bindings.contains_key("send")
        {
            continue;
        }
        let values: Vec<TsNode<'_>> = named_children(arguments)
            .into_iter()
            .filter(|argument| argument.kind() == "value_argument")
            .collect();
        let args: Vec<TsNode<'_>> = values
            .iter()
            .filter_map(|value| match named_children(*value).as_slice() {
                [expression] => Some(*expression),
                _ => None,
            })
            .collect();
        // Named arguments are not mapped to parameters, and `send` takes one
        // to five arguments (topic, partition, timestamp, key, data).
        if args.is_empty() || args.len() > 5 || args.len() != values.len() {
            continue;
        }
        let Some(symbol) = enclosing(functions, call) else {
            continue;
        };
        if !proof.is_client(call, receiver, KAFKA_TEMPLATE, true) {
            continue;
        }
        let identity = match args.as_slice() {
            [_] => IdentityExpr::Computed(cx.text(&call).to_string()),
            [first, ..] => topic(cx, *first),
            [] => unreachable!("checked above"),
        };
        out.push(site(cx, call, ChannelRole::Produces, identity, symbol));
    }
    out.sort_by_key(|site| site.byte_start);
    out
}
