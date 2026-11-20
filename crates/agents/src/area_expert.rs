//! The topic expert's request and reply, as pure functions (#2057, AC-0932).
//!
//! `topic-expert@1` answers one question about one subsystem area in one
//! bounded call. It is handed the area's compiled briefing (#2055): that
//! area's part of the product specification, with the citation ids the
//! specification prints. It calls no tool and reads nothing else.
//!
//! **The request.** [`action`] writes the one completion action: fixed
//! instructions, the briefing, the earlier turns of the conversation and
//! the question. The host's bounded call path applies the egress policy,
//! redaction, consent and limits to it unchanged.
//!
//! **The reply.** [`decode`] reads the reply into an [`ExpertAnswer`]. A
//! reply that is not the schema's object refuses with a closed reason. A
//! statement is kept only with the citations the briefing prints; one left
//! with none, or that breaks a statement bound, is left out and counted. An
//! area to ask instead is kept only when the briefing names it as a
//! neighbour. An answer may hold no statement: then it says only what is
//! not known.

use crate::section_draft::statement_admissible;
use llm::{AnalysisTier, CompletionAction, CompletionPayload};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt::Write as _;

/// The author role of an area answer.
pub const TOPIC_EXPERT_ROLE: &str = "topic-expert@1";

/// Most statements an answer holds.
pub const MAX_STATEMENTS: usize = 24;
/// Most citations one statement keeps.
pub const MAX_STATEMENT_CITATIONS: usize = 12;
/// Most entries of the not-known list.
pub const MAX_NOT_KNOWN: usize = 12;
/// Longest question, in bytes.
pub const MAX_QUESTION_BYTES: usize = 2_048;
/// Most earlier turns sent with a question.
pub const MAX_TURNS: usize = 6;
/// Longest earlier answer sent back, in bytes; a longer one is cut.
pub const MAX_TURN_ANSWER_BYTES: usize = 4_096;

const SYSTEM: &str = "You are Cartograph's topic expert for one area of a legacy system. \
You answer a developer who must understand or rebuild that area and has never seen the \
code. You are given the area's briefing: its part of the product specification that \
Cartograph compiled from the system's knowledge graph (capabilities with their operations, \
acceptance criteria, business rules and business flows), with the citation ids (F-…) the \
specification prints. Answer the question from the briefing only. Write plain, concrete \
statements in domain terms: what happens, for whom, under which conditions, with which \
limits, statuses and messages. Every statement cites the ids of the briefing rows it rests \
on; never cite an id the briefing does not print. Mark a statement `stated` when the \
briefing says it and `inferred` when it is your reading across rows. Do not answer from \
general knowledge of similar systems. If the briefing does not hold something the question \
needs, say so in not_known instead of guessing; a row the briefing marks as a Gap or as not \
recovered is not known. If the question belongs to a neighbouring area the briefing names, \
list that area in ask_instead and do not describe it. The area is an inferred grouping, not \
a confirmed boundary; do not present it as one. Reply with the JSON object the response \
schema describes and nothing else.";

/// One earlier exchange of the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpertTurn {
    /// What was asked.
    pub question: String,
    /// What was answered, as plain text.
    pub answer: String,
}

/// Everything one answer is written from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpertInput<'a> {
    /// The area's name.
    pub area: &'a str,
    /// The area's compiled briefing.
    pub briefing: &'a str,
    /// Earlier turns, oldest first; only the last [`MAX_TURNS`] are sent.
    pub turns: &'a [ExpertTurn],
    /// The question.
    pub question: &'a str,
}

/// Whether `question` may be asked: not blank, bounded, no control
/// character other than a line break.
pub fn question_admissible(question: &str) -> bool {
    question.len() <= MAX_QUESTION_BYTES
        && question.chars().any(char::is_alphanumeric)
        && !question.chars().any(|c| c.is_control() && c != '\n')
}

/// The fingerprint of this role's definition: its instructions and schema.
pub fn definition_fingerprint() -> String {
    crate::source_basis::domain_hash(
        b"cartograph:topic-expert-definition:v1\0",
        format!("{TOPIC_EXPERT_ROLE}\0{SYSTEM}\0{}", response_schema()).as_bytes(),
    )
}

/// The response schema. Citation ids are checked against the briefing in
/// [`decode`], not listed here: a briefing prints too many for an enum.
pub fn response_schema() -> String {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["answer", "not_known", "ask_instead"],
        "properties": {
            "answer": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["statement", "basis", "citation_ids"],
                    "properties": {
                        "statement": {"type": "string"},
                        "basis": {"enum": ["stated", "inferred"]},
                        "citation_ids": {"type": "array", "items": {"type": "string"}}
                    }
                }
            },
            "not_known": {"type": "array", "items": {"type": "string"}},
            "ask_instead": {"type": "array", "items": {"type": "string"}}
        }
    })
    .to_string()
}

/// `text` cut to at most `max` bytes on a character boundary.
fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn prompt(input: &ExpertInput<'_>) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "You are the topic expert for the area \"{}\".\n\
         Answer in at most {MAX_STATEMENTS} statements, each citing 1 to \
         {MAX_STATEMENT_CITATIONS} citation ids from the briefing.\n\n\
         <briefing>\n{}\n</briefing>\n",
        input.area,
        input.briefing.trim_end(),
    );
    let from = input.turns.len().saturating_sub(MAX_TURNS);
    let turns = &input.turns[from..];
    if !turns.is_empty() {
        out.push_str(
            "\n<earlier_turns>\nEarlier questions in this conversation and your answers. \
             They are context, not evidence: cite the briefing, not these.\n",
        );
        for turn in turns {
            let _ = writeln!(
                out,
                "Q: {}\nA: {}",
                cut(turn.question.trim(), MAX_QUESTION_BYTES),
                cut(turn.answer.trim(), MAX_TURN_ANSWER_BYTES)
            );
        }
        out.push_str("</earlier_turns>\n");
    }
    let _ = write!(
        out,
        "\n<question>\n{}\n</question>\n",
        input.question.trim()
    );
    out
}

/// The one completion action of an answer.
pub fn action(input: &ExpertInput<'_>, action_id: String) -> CompletionAction {
    CompletionAction {
        action_id,
        tier: AnalysisTier::Agentic,
        payload: CompletionPayload {
            response_schema: Some(response_schema()),
            system: SYSTEM.to_owned(),
            prompt: prompt(input),
            spans: Vec::new(),
        },
    }
}

/// Whether the briefing says a statement or the expert read it across rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// The briefing states it.
    Stated,
    /// A reading across briefing rows.
    Inferred,
}

/// One cited statement of an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpertStatement {
    /// The statement, plain text.
    pub statement: String,
    /// Its basis.
    pub basis: Basis,
    /// The briefing citation ids it rests on, in the order given.
    pub citations: Vec<String>,
}

/// A decoded answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpertAnswer {
    /// The cited statements, in order.
    pub statements: Vec<ExpertStatement>,
    /// What the question needed and the briefing does not hold.
    pub not_known: Vec<String>,
    /// Neighbouring areas to ask instead.
    pub ask_instead: Vec<String>,
    /// Statements left out: no citation the briefing prints, or a statement
    /// bound.
    pub left_out: usize,
}

/// Why a reply yields no answer. Closed: never text from the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpertRefusal {
    /// The reply is not the schema's JSON object.
    NotTheSchema,
    /// Nothing is left: no statement and nothing stated as not known.
    Empty,
}

impl ExpertRefusal {
    /// The reason code.
    pub fn code(self) -> &'static str {
        match self {
            Self::NotTheSchema => "reply_not_the_schema",
            Self::Empty => "no_admissible_statement",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    answer: Vec<ReplyStatement>,
    not_known: Vec<String>,
    ask_instead: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyStatement {
    statement: String,
    basis: Basis,
    citation_ids: Vec<String>,
}

/// Layout whitespace read as a space; the result trimmed.
fn plain(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .collect();
    flat.trim().to_owned()
}

/// Decode `text`, the reply to [`action`], keeping only citation ids in
/// `citable` (the ids the briefing prints) and only areas in `neighbours`.
pub fn decode(
    text: &str,
    citable: &BTreeSet<String>,
    neighbours: &BTreeSet<String>,
) -> Result<ExpertAnswer, ExpertRefusal> {
    let reply: Reply =
        serde_json::from_str(text.trim()).map_err(|_| ExpertRefusal::NotTheSchema)?;
    let mut statements = Vec::new();
    let mut left_out = 0;
    for item in reply.answer {
        let mut citations: Vec<String> = Vec::new();
        for id in item.citation_ids {
            if citable.contains(&id)
                && !citations.contains(&id)
                && citations.len() < MAX_STATEMENT_CITATIONS
            {
                citations.push(id);
            }
        }
        let statement = plain(&item.statement);
        if citations.is_empty()
            || !statement_admissible(&statement)
            || statements.len() >= MAX_STATEMENTS
        {
            left_out += 1;
            continue;
        }
        statements.push(ExpertStatement {
            statement,
            basis: item.basis,
            citations,
        });
    }
    let not_known: Vec<String> = reply
        .not_known
        .iter()
        .map(|entry| plain(entry))
        .filter(|entry| statement_admissible(entry))
        .take(MAX_NOT_KNOWN)
        .collect();
    let mut ask_instead: Vec<String> = Vec::new();
    for area in reply.ask_instead {
        let area = plain(&area);
        if neighbours.contains(&area) && !ask_instead.contains(&area) {
            ask_instead.push(area);
        }
    }
    if statements.is_empty() && not_known.is_empty() && ask_instead.is_empty() {
        return Err(ExpertRefusal::Empty);
    }
    Ok(ExpertAnswer {
        statements,
        not_known,
        ask_instead,
        left_out,
    })
}

#[cfg(test)]
#[path = "area_expert_tests.rs"]
mod tests;
