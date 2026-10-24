//! Borrow request text until the existing bounded writer admits its encoding.
//! Field order preserves the compact JSON emitted by the former Value maps.
use serde::Serialize;

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
struct Ollama<'a> {
    format: &'static str,
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
) -> impl Serialize + 'a {
    Ollama {
        format: "json",
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
        think: false,
    }
}

#[derive(Serialize)]
struct Anthropic<'a> {
    max_tokens: u32,
    messages: [Message<'a>; 1],
    model: &'a str,
    stream: bool,
    system: &'a str,
}

pub(super) fn anthropic<'a>(
    model: &'a str,
    system: &'a str,
    user: &'a str,
    max_tokens: u32,
) -> impl Serialize + 'a {
    Anthropic {
        max_tokens,
        messages: [Message {
            content: user,
            role: "user",
        }],
        model,
        stream: false,
        system,
    }
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct OpenAiCompatible<'a> {
    max_tokens: u32,
    messages: [Message<'a>; 2],
    model: &'a str,
    response_format: ResponseFormat,
    stream: bool,
}

pub(super) fn openai_compatible<'a>(
    model: &'a str,
    system: &'a str,
    user: &'a str,
    max_tokens: u32,
) -> impl Serialize + 'a {
    OpenAiCompatible {
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
            kind: "json_object",
        },
        stream: false,
    }
}

#[cfg(test)]
mod tests;
