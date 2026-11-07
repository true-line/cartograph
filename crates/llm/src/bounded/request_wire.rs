//! Borrow request text until the existing bounded writer admits its encoding.
//! Field order preserves the compact JSON emitted by the former Value maps.
use serde::Serialize;

mod anthropic_schema;
mod openai_schema;
pub(super) use anthropic_schema::project as anthropic_schema;
pub(super) use openai_schema::OpenAiSchemaFormatter;

#[derive(Serialize)]
struct Message<'a> {
    content: &'a str,
    role: &'static str,
}

#[derive(Serialize)]
struct Options {
    num_predict: u32,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Format<'a> {
    Json(&'static str),
    Schema(&'a serde_json::value::RawValue),
}

#[derive(Serialize)]
struct Ollama<'a> {
    format: Format<'a>,
    messages: [Message<'a>; 2],
    model: &'a str,
    options: Options,
    stream: bool,
    think: bool,
}

pub(super) fn ollama<'a>(
    model: &'a str,
    system: &'a str,
    user: &'a str,
    max_tokens: u32,
    schema: Option<&'a serde_json::value::RawValue>,
    think: bool,
) -> impl Serialize + 'a {
    Ollama {
        format: schema.map_or(Format::Json("json"), Format::Schema),
        messages: [
            Message {
                content: system,
                role: "system",
            },
            Message {
                content: user,
                role: "user",
            },
        ],
        model,
        options: Options {
            num_predict: max_tokens,
        },
        stream: false,
        // Advisory only; full-body and time limits remain authoritative.
        think,
    }
}

#[derive(Serialize)]
struct AnthropicFormat<'a> {
    schema: &'a serde_json::Value,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct AnthropicOutput<'a> {
    format: AnthropicFormat<'a>,
}

#[derive(Serialize)]
struct Anthropic<'a> {
    max_tokens: u32,
    messages: [Message<'a>; 1],
    model: &'a str,
    // Absent without a schema, so schema-free request bytes are unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<AnthropicOutput<'a>>,
    stream: bool,
    system: &'a str,
}

/// `schema` must already be the [`anthropic_schema`] wire projection.
pub(super) fn anthropic<'a>(
    model: &'a str,
    system: &'a str,
    user: &'a str,
    max_tokens: u32,
    schema: Option<&'a serde_json::Value>,
) -> impl Serialize + 'a {
    Anthropic {
        max_tokens,
        messages: [Message {
            content: user,
            role: "user",
        }],
        model,
        output_config: schema.map(|schema| AnthropicOutput {
            format: AnthropicFormat {
                schema,
                kind: "json_schema",
            },
        }),
        stream: false,
        system,
    }
}

#[derive(Serialize)]
struct JsonSchema<'a> {
    name: &'static str,
    strict: bool,
    schema: &'a serde_json::value::RawValue,
}

#[derive(Serialize)]
struct ResponseFormat<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    json_schema: Option<JsonSchema<'a>>,
}

#[derive(Serialize)]
struct ChatTemplate {
    enable_thinking: bool,
}

#[derive(Serialize)]
struct OpenAiCompatible<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<ChatTemplate>,
    max_tokens: u32,
    messages: [Message<'a>; 2],
    model: &'a str,
    response_format: ResponseFormat<'a>,
    stream: bool,
}

pub(super) fn openai_compatible<'a>(
    model: &'a str,
    system: &'a str,
    user: &'a str,
    max_tokens: u32,
    schema: Option<&'a serde_json::value::RawValue>,
    thinking: Option<bool>,
) -> impl Serialize + 'a {
    OpenAiCompatible {
        chat_template_kwargs: thinking.map(|enable_thinking| ChatTemplate { enable_thinking }),
        max_tokens,
        messages: [
            Message {
                content: system,
                role: "system",
            },
            Message {
                content: user,
                role: "user",
            },
        ],
        model,
        response_format: ResponseFormat {
            kind: if schema.is_some() {
                "json_schema"
            } else {
                "json_object"
            },
            json_schema: schema.map(|schema| JsonSchema {
                name: "cartograph_bounded_response",
                strict: true,
                schema,
            }),
        },
        stream: false,
    }
}

#[cfg(test)]
mod tests;
