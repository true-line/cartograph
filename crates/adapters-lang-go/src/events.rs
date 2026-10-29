//! Event SDK request and config structs (#1349, AC-0462).
//!
//! Go method receivers are not typed at T0, but an SDK's request or config
//! struct names its channel: a composite literal of an import-proven package
//! type is a site, as a TypeScript `new SendMessageCommand({ QueueUrl })` is.
//! The package qualifier must be the file's import of the exact SDK path,
//! with no local value or type of that name in the top-level declaration
//! around it (the AC-0434 `net/http` proof).
//!
//! The identity is the keyed identity field: an exact string literal for a
//! `string` field (kafka-go), or `aws.String("…")` with `aws` the SDK's own
//! `aws` import for a `*string` field (AWS); anything else is computed, and
//! a literal without the field is one computed site named by its text (a
//! kafka-go `Writer` without `Topic` sets topics per message). Unkeyed
//! literals are not read. No registry entry shapes these sites.

use super::*;
use adapters_fw::events::{ChannelRole, EventSite, IdentityExpr};
use client::{shadowed, top_level};

/// `(import paths, type, kind, role, identity field)`.
type Shape = (
    &'static [&'static str],
    &'static str,
    &'static str,
    ChannelRole,
    &'static str,
);

const SQS: &[&str] = &[
    "github.com/aws/aws-sdk-go-v2/service/sqs",
    "github.com/aws/aws-sdk-go/service/sqs",
];
const SNS: &[&str] = &[
    "github.com/aws/aws-sdk-go-v2/service/sns",
    "github.com/aws/aws-sdk-go/service/sns",
];
const KAFKA_GO: &[&str] = &["github.com/segmentio/kafka-go"];
/// The `aws` helper packages whose `String` makes a `*string`.
const AWS: &[&str] = &[
    "github.com/aws/aws-sdk-go-v2/aws",
    "github.com/aws/aws-sdk-go/aws",
];

const SHAPES: &[Shape] = &[
    (
        SQS,
        "SendMessageInput",
        "sqs-queue",
        ChannelRole::Produces,
        "QueueUrl",
    ),
    (
        SQS,
        "ReceiveMessageInput",
        "sqs-queue",
        ChannelRole::Consumes,
        "QueueUrl",
    ),
    (
        SNS,
        "PublishInput",
        "sns-topic",
        ChannelRole::Produces,
        "TopicArn",
    ),
    (
        KAFKA_GO,
        "Writer",
        "kafka-topic",
        ChannelRole::Produces,
        "Topic",
    ),
    (
        KAFKA_GO,
        "WriterConfig",
        "kafka-topic",
        ChannelRole::Produces,
        "Topic",
    ),
    (
        KAFKA_GO,
        "ReaderConfig",
        "kafka-topic",
        ChannelRole::Consumes,
        "Topic",
    ),
];

/// Whether identifier text `name` at `at` is the file's import of one of
/// `paths`.
fn imports(
    cx: &FileCx<'_>,
    bindings: &HashMap<String, ImportBinding>,
    name: &str,
    at: TsNode<'_>,
    paths: &[&str],
) -> bool {
    bindings
        .get(name)
        .is_some_and(|binding| paths.contains(&binding.path.as_str()))
        && !shadowed(cx, top_level(at), name)
}

/// An exact string literal's value: raw, or interpreted with no escape.
fn exact(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    let text = cx.text(&node);
    match node.kind() {
        "raw_string_literal" if !text.contains('\r') => unquote(text),
        "interpreted_string_literal" if !text.contains('\\') => unquote(text),
        _ => None,
    }
}

/// The identity a field value gives: a literal for a `string` field, or an
/// `aws.String` of one for a `*string` field.
fn identity(
    cx: &FileCx<'_>,
    bindings: &HashMap<String, ImportBinding>,
    value: TsNode<'_>,
    pointer: bool,
) -> IdentityExpr {
    let literal = if pointer {
        (value.kind() == "call_expression")
            .then(|| {
                let callee = value.child_by_field_name("function")?;
                let operand = callee.child_by_field_name("operand")?;
                let field = callee.child_by_field_name("field")?;
                let [argument] = call_arguments(value)[..] else {
                    return None;
                };
                (callee.kind() == "selector_expression"
                    && operand.kind() == "identifier"
                    && cx.text(&field) == "String"
                    && imports(cx, bindings, cx.text(&operand), value, AWS))
                .then(|| exact(cx, argument))
                .flatten()
            })
            .flatten()
    } else {
        exact(cx, value)
    };
    match literal {
        Some(literal) => IdentityExpr::Literal(literal),
        None => IdentityExpr::Computed(cx.text(&value).trim().to_string()),
    }
}

/// The event site a composite literal is, if its type is a proven shape.
pub(super) fn site(
    cx: &FileCx<'_>,
    bindings: &HashMap<String, ImportBinding>,
    literal: TsNode<'_>,
    symbol: Option<String>,
) -> Option<EventSite> {
    let ty = literal.child_by_field_name("type")?;
    let body = literal.child_by_field_name("body")?;
    if ty.kind() != "qualified_type" {
        return None;
    }
    let package = cx.text(&ty.child_by_field_name("package")?);
    let name = cx.text(&ty.child_by_field_name("name")?);
    let (paths, _, kind, role, field) = SHAPES.iter().find(|(paths, shape, ..)| {
        *shape == name && imports(cx, bindings, package, literal, paths)
    })?;
    let mut walk = body.walk();
    let elements: Vec<TsNode<'_>> = body
        .named_children(&mut walk)
        .filter(|element| element.kind() != "comment")
        .collect();
    // An unkeyed literal sets fields by position: not read.
    if elements
        .iter()
        .any(|element| element.kind() != "keyed_element")
    {
        return None;
    }
    let value = elements.iter().find_map(|element| {
        let key = element.child_by_field_name("key")?;
        let key = key.named_child(0).unwrap_or(key);
        (key.kind() == "identifier" && cx.text(&key) == *field)
            .then(|| element.child_by_field_name("value"))
            .flatten()
            .map(|value| value.named_child(0).unwrap_or(value))
    });
    let identity = match value {
        Some(value) => identity(cx, bindings, value, *paths != KAFKA_GO),
        None => IdentityExpr::Computed(cx.text(&literal).trim().to_string()),
    };
    Some(EventSite {
        kind: kind.to_string(),
        role: *role,
        identity,
        symbol,
        path: cx.path.to_string(),
        byte_start: literal.start_byte() as u64,
        byte_end: literal.end_byte() as u64,
        components: Default::default(),
        batch: None,
        registries: Vec::new(),
    })
}
