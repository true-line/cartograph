//! rdkafka event sites (#1371, AC-0469).
//!
//! A producer is `FutureRecord::to(topic)` or `BaseRecord::to(topic)` whose
//! path resolves, through `use` or a qualified path, to `rdkafka::producer`
//! (or its `future_producer` / `base_producer` module). A turbofish record
//! (`FutureRecord::<K, V>::to`) is not read.
//!
//! A consumer is `<consumer>.subscribe(&[…])` on a receiver proven to be an
//! rdkafka `StreamConsumer` or `BaseConsumer` as [`Proof`] proves a reqwest
//! `Client`: a name the function binds once with that type written (generic
//! arguments aside, behind `&` or `Arc` / `Rc`), a parameter so typed, or
//! `self.<field>` of a struct this module declares with that field so
//! typed. `ClientConfig::create()` proves nothing by itself, since its type
//! is inferred, and neither does `unwrap()`/`expect()` on a proven consumer.
//! Each element of an array literal is one site; a repeat array or any other
//! argument is one computed site. `assign(…)` is not read.
//!
//! A topic is a literal only as a string literal (behind `&`); anything
//! else is computed.

use super::*;
use adapters_fw::events::{ChannelRole, EventSite, IdentityExpr};
use http::{call_arguments, unquote_string_literal};
use reqwest::{ClientFields, Proof, typed_fields};

const KIND: &str = "kafka-topic";

fn is_record_constructor(path: &[String]) -> bool {
    matches!(
        path.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["rdkafka", "producer", "FutureRecord" | "BaseRecord", "to"]
            | [
                "rdkafka",
                "producer",
                "future_producer",
                "FutureRecord",
                "to"
            ]
            | ["rdkafka", "producer", "base_producer", "BaseRecord", "to"]
    )
}

fn is_consumer_path(path: &[String]) -> bool {
    matches!(
        path.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["rdkafka", "consumer", "StreamConsumer" | "BaseConsumer"]
            | ["rdkafka", "consumer", "stream_consumer", "StreamConsumer"]
            | ["rdkafka", "consumer", "base_consumer", "BaseConsumer"]
    )
}

/// Struct fields typed as an rdkafka consumer, for `self.<field>` receivers.
pub(super) fn consumer_fields(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &Bindings,
) -> ClientFields {
    typed_fields(cx, root, bindings, is_consumer_path)
}

/// `node` without a leading `&`.
fn unref(node: TsNode<'_>) -> TsNode<'_> {
    if node.kind() == "reference_expression" {
        node.child_by_field_name("value").unwrap_or(node)
    } else {
        node
    }
}

fn topic(cx: &FileCx<'_>, node: TsNode<'_>) -> IdentityExpr {
    let node = unref(node);
    match unquote_string_literal(cx, node) {
        Some(literal) => IdentityExpr::Literal(literal),
        None => IdentityExpr::Computed(cx.text(&node).trim().to_string()),
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

/// rdkafka event sites in `function` (nested functions excluded).
pub(super) fn extract_function_sites(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    symbol: &str,
    bindings: &Bindings,
    consumer_fields: &ClientFields,
    out: &mut Extraction,
) {
    let proof = Proof::new(
        cx,
        bindings,
        function,
        consumer_fields,
        is_consumer_path,
        false,
    );
    let mut calls = Vec::new();
    let mut stack = vec![function];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression" {
            calls.push(node);
        }
        let mut walk = node.walk();
        for child in node.named_children(&mut walk) {
            if child.kind() != "function_item" {
                stack.push(child);
            }
        }
    }
    calls.sort_by_key(|call| call.start_byte());
    for call in calls {
        let Some(callee) = call.child_by_field_name("function") else {
            continue;
        };
        let args = call_arguments(call);
        match callee.kind() {
            // A turbofish (`FutureRecord::<K, V>::to`) is not read.
            "scoped_identifier" if !cx.text(&callee).contains('<') => {
                let [topic_arg] = args.as_slice() else {
                    continue;
                };
                if proof
                    .path(callee, cx.text(&callee))
                    .is_some_and(|path| is_record_constructor(&path))
                {
                    let identity = topic(cx, *topic_arg);
                    out.event_sites
                        .push(site(cx, call, ChannelRole::Produces, identity, symbol));
                }
            }
            "field_expression" => {
                let (Some(receiver), Some(method)) = (
                    callee.child_by_field_name("value"),
                    callee.child_by_field_name("field"),
                ) else {
                    continue;
                };
                let [topics] = args.as_slice() else {
                    continue;
                };
                if cx.text(&method) != "subscribe" || !proof.is_client_expr(receiver) {
                    continue;
                }
                let list = unref(*topics);
                // A repeat array (`[t; n]`) may hold no topic at all.
                if list.kind() == "array_expression" && list.child_by_field_name("length").is_none()
                {
                    let mut walk = list.walk();
                    for element in list.named_children(&mut walk) {
                        let identity = topic(cx, element);
                        out.event_sites.push(site(
                            cx,
                            element,
                            ChannelRole::Consumes,
                            identity,
                            symbol,
                        ));
                    }
                } else {
                    let identity = IdentityExpr::Computed(cx.text(&list).trim().to_string());
                    out.event_sites
                        .push(site(cx, call, ChannelRole::Consumes, identity, symbol));
                }
            }
            _ => {}
        }
    }
}
