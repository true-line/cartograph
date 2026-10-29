//! Spring Kafka event sites (#1347, AC-0459).
//!
//! A producer is `<template>.send(…)` on a receiver proven to be
//! `org.springframework.kafka.core.KafkaTemplate` as [`rest_template::Proof`]
//! proves a `RestTemplate`: a local, parameter or own-type field (or
//! `this.x`) whose declared type, generic arguments aside, is spelled by the
//! fully qualified name or the simple name under its single-type import.
//! Subclasses (`ReplyingKafkaTemplate`, …) and `sendDefault` are not read.
//! The topic is the first of two or more arguments; a one-argument
//! `send(ProducerRecord)` or `send(Message)` carries it at runtime.
//!
//! A consumer is a method annotated `@KafkaListener`, spelled by its fully
//! qualified name or under its single-type import, with one site per
//! `topics` element. A `topicPattern` or `topicPartitions` listener is one
//! computed site. Class-level listeners and their `@KafkaHandler` methods
//! are not read.
//!
//! A topic is a literal only as an exact string literal holding no `${…}`
//! property placeholder or `#{…}` expression; anything else is computed.
//! No registry entry shapes these sites.

use super::*;
use adapters_fw::events::{ChannelRole, EventSite, IdentityExpr};
use rest_template::{Proof, enclosing};

const KAFKA_TEMPLATE: &str = "org.springframework.kafka.core.KafkaTemplate";
const KAFKA_LISTENER: &str = "org.springframework.kafka.annotation.KafkaListener";
const KIND: &str = "kafka-topic";

/// A topic expression, classified.
fn topic(cx: &FileCx<'_>, node: TsNode<'_>) -> IdentityExpr {
    match string_literal(cx, node) {
        Some(topic) if !topic.contains("${") && !topic.contains("#{") => {
            IdentityExpr::Literal(topic)
        }
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

/// The topics a `@KafkaListener` names: each `topics` element, or the
/// whole pattern or partition argument as one computed topic.
fn listener_topics<'t>(cx: &FileCx<'_>, annotation: TsNode<'t>) -> Vec<(TsNode<'t>, IdentityExpr)> {
    let Some(arguments) = annotation.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut topics = Vec::new();
    let mut walk = arguments.walk();
    for argument in arguments.named_children(&mut walk) {
        let (Some(key), Some(value)) = (
            argument.child_by_field_name("key"),
            argument.child_by_field_name("value"),
        ) else {
            continue;
        };
        match cx.text(&key) {
            "topics" if value.kind() == "element_value_array_initializer" => {
                let mut inner = value.walk();
                topics.extend(
                    value
                        .named_children(&mut inner)
                        .filter(|element| !is_comment(*element))
                        .map(|element| (element, topic(cx, element))),
                );
            }
            "topics" => topics.push((value, topic(cx, value))),
            "topicPattern" | "topicPartitions" => topics.push((
                argument,
                IdentityExpr::Computed(cx.text(&argument).to_string()),
            )),
            _ => {}
        }
    }
    topics
}

/// Spring Kafka event sites in this file, owned by their enclosing method
/// or constructor (producers) or their annotated method (consumers).
pub(crate) fn sites(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    imports: &Imports,
    methods: &[(TsNode<'_>, String, Vec<String>, String)],
    methods_by_start: &HashMap<usize, String>,
) -> Vec<EventSite> {
    let text = cx.text(&root);
    if !text.contains("KafkaTemplate") && !text.contains("KafkaListener") {
        return Vec::new();
    }
    let proof = Proof::new(cx, root, imports);
    let mut out = Vec::new();
    for (method, symbol, _, _) in methods {
        for (name, annotation) in declaration_annotations(cx, *method) {
            let spelled = annotation
                .child_by_field_name("name")
                .map(|spelled| cx.text(&spelled))
                .unwrap_or_default();
            if name != "KafkaListener" || !proof.names(spelled, KAFKA_LISTENER) {
                continue;
            }
            for (node, identity) in listener_topics(cx, annotation) {
                out.push(site(cx, node, ChannelRole::Consumes, identity, symbol));
            }
        }
    }
    let query = crate::queries::query("(method_invocation) @call");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, cx.source);
    while let Some(found) = matches.next() {
        let call = found.captures()[0].node;
        let (Some(object), Some(name), Some(arguments)) = (
            call.child_by_field_name("object"),
            call.child_by_field_name("name"),
            call.child_by_field_name("arguments"),
        ) else {
            continue;
        };
        if cx.text(&name) != "send" {
            continue;
        }
        let mut walk = arguments.walk();
        let args = arguments
            .named_children(&mut walk)
            .filter(|argument| !is_comment(*argument))
            .collect::<Vec<_>>();
        let Some((method, symbol)) = enclosing(call, methods_by_start) else {
            continue;
        };
        if args.is_empty() || !proof.is_client(method, call, object, KAFKA_TEMPLATE) {
            continue;
        }
        let identity = match args.as_slice() {
            [_] => IdentityExpr::Computed(cx.text(&call).to_string()),
            [first, ..] => topic(cx, *first),
            [] => unreachable!("checked above"),
        };
        out.push(site(cx, call, ChannelRole::Produces, identity, &symbol));
    }
    out.sort_by_key(|site| site.byte_start);
    out
}
