//! The rule-scope check's request and reply, as pure functions (#2482,
//! #2495).
//!
//! `rule-scope@2` reads, for each business rule a claim cites, the claim
//! beside that rule and the capabilities the rule holds for, and answers
//! `widened` when the claim states the rule's condition, message or outcome
//! for anything beyond them (a sign-in error of one provider stated for
//! every provider, a custom-alias limit stated for random aliases too), or
//! `within`. A claim most samples read as widening any of its rules is left
//! out. It calls no tool and writes nothing.
//!
//! `rule-scope@1` asked instead which of the claim's cited capabilities the
//! claim states each rule for; on a local model its replies listed every
//! capability of the claim, answered `all` throughout, or named the rule's
//! own capability whatever the claim said (re-check 11), so it judged the
//! reply's style rather than the claim.

use crate::claim_review::FACT_TEXT_CHARS;
use llm::{AnalysisTier, CompletionAction, CompletionPayload};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// The role's name and version.
pub const RULE_SCOPE_ROLE: &str = "rule-scope@2";

/// The answer for a claim stating a rule beyond its capabilities.
pub const WIDENED: &str = "widened";

/// The answer for a claim stating a rule only for its capabilities.
pub const WITHIN: &str = "within";

/// How many times the same request is asked; a rule is read as widened on a
/// majority of the readable replies.
pub const SCOPE_SAMPLES: usize = 3;

const SYSTEM: &str = "You read claims of a product specification beside business rules they \
cite. Each rule holds only for the capabilities it is listed under. For each claim and rule \
pair, answer \"widened\" when the claim states that rule's condition, message or outcome for \
anything beyond those capabilities: another provider, form, page, API call, caller group or \
kind of item, or every one of a kind. Otherwise answer \"within\", including when the claim \
names other capabilities for other reasons in the same sentence. Judge only what the claim \
says this rule applies to.";

/// One cited rule: its citation id, the rule in domain terms and the names
/// of the capabilities it holds for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRule<'a> {
    /// The rule's citation id.
    pub id: &'a str,
    /// The rule in domain terms.
    pub text: &'a str,
    /// The capabilities it holds for: those it is listed under, and those
    /// an identical rule is listed under.
    pub holds_for: Vec<&'a str>,
}

/// One claim to check: its statement, every id it cites and the rules it
/// cites that have a capability to hold for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeClaim<'a> {
    /// The claim's statement.
    pub statement: &'a str,
    /// Every citation id of the claim.
    pub citations: &'a [String],
    /// The rules it cites.
    pub rules: Vec<ScopeRule<'a>>,
}

/// The label of the `rule`th rule (from 1) of the `claim`th claim.
fn label(claim: usize, rule: usize) -> String {
    format!("{claim}.{rule}")
}

fn labels(claims: &[ScopeClaim<'_>]) -> Vec<String> {
    claims
        .iter()
        .enumerate()
        .flat_map(|(n, claim)| (0..claim.rules.len()).map(move |r| label(n + 1, r + 1)))
        .collect()
}

/// The response schema: exactly one answer for each claim and rule pair.
pub fn response_schema(claims: &[ScopeClaim<'_>]) -> String {
    let pairs = labels(claims);
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["answers"],
        "properties": {
            "answers": {
                "type": "array",
                "minItems": pairs.len(),
                "maxItems": pairs.len(),
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["pair", "answer"],
                    "properties": {
                        "pair": {"enum": pairs},
                        "answer": {"enum": [WITHIN, WIDENED]}
                    }
                }
            }
        }
    })
    .to_string()
}

/// The fingerprint of this role's definition: its instructions and the
/// version of its schema.
pub fn definition_fingerprint() -> String {
    crate::source_basis::domain_hash(
        b"cartograph:rule-scope-definition:v2\0",
        format!("{RULE_SCOPE_ROLE}\0{SYSTEM}\0{WITHIN}\0{WIDENED}").as_bytes(),
    )
}

/// The one completion action checking `claims`, with `facts` the text of
/// each cited fact by citation id, printed once and cut like the
/// reviewer's, then one line per claim and rule pair.
pub fn action(
    claims: &[ScopeClaim<'_>],
    facts: &BTreeMap<String, String>,
    action_id: String,
) -> CompletionAction {
    let mut prompt = String::from("<facts>\n");
    let cited: BTreeSet<&String> = claims.iter().flat_map(|claim| claim.citations).collect();
    for id in cited {
        let text = facts.get(id).map_or("(no text)", String::as_str);
        let cut: String = text.chars().take(FACT_TEXT_CHARS).collect();
        let _ = writeln!(prompt, "{id}: {}", cut.replace('\n', " "));
    }
    prompt.push_str("</facts>\n\n<pairs>\n");
    for (n, claim) in claims.iter().enumerate() {
        let statement = claim.statement.replace('\n', " ");
        for (r, rule) in claim.rules.iter().enumerate() {
            let holds: Vec<String> = rule
                .holds_for
                .iter()
                .map(|name| format!("\"{name}\""))
                .collect();
            let _ = writeln!(
                prompt,
                "{}  Claim {}: \"{statement}\"  |  Rule {} of {}: {}",
                label(n + 1, r + 1),
                n + 1,
                rule.id,
                holds.join(", "),
                rule.text.replace('\n', " ")
            );
        }
    }
    let pairs: usize = claims.iter().map(|claim| claim.rules.len()).sum();
    let _ = write!(prompt, "</pairs>\n\nAnswer each pair once: {pairs} in all.");
    CompletionAction {
        action_id,
        tier: AnalysisTier::Agentic,
        payload: CompletionPayload {
            response_schema: Some(response_schema(claims)),
            system: SYSTEM.to_owned(),
            prompt,
            spans: Vec::new(),
        },
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    answers: Vec<ReplyAnswer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyAnswer {
    pair: String,
    answer: String,
}

/// Whether each claim widens each of its rules, by claim then rule, or
/// `None` when the reply is not the schema's object, misses, repeats or
/// invents a pair, or answers `widened` for every one of two or more pairs
/// (a reply that does not tell the pairs apart).
pub fn decode(text: &str, claims: &[ScopeClaim<'_>]) -> Option<Vec<Vec<bool>>> {
    let reply: Reply = serde_json::from_str(text.trim()).ok()?;
    let expected: BTreeSet<String> = labels(claims).into_iter().collect();
    let mut given: BTreeMap<String, bool> = BTreeMap::new();
    for answer in reply.answers {
        let widened = match answer.answer.as_str() {
            WIDENED => true,
            WITHIN => false,
            _ => return None,
        };
        if !expected.contains(&answer.pair) || given.insert(answer.pair, widened).is_some() {
            return None;
        }
    }
    if given.len() != expected.len() || (given.len() > 1 && given.values().all(|w| *w)) {
        return None;
    }
    Some(
        claims
            .iter()
            .enumerate()
            .map(|(n, claim)| {
                (0..claim.rules.len())
                    .map(|r| given[&label(n + 1, r + 1)])
                    .collect()
            })
            .collect(),
    )
}

/// Which claims widen a rule, on a majority of the readable `samples` (at
/// least two): a claim is flagged when any of its rules is read as widened
/// by more than half of them. `None` when fewer than two samples could be
/// read.
pub fn widened_claims(samples: &[Option<Vec<Vec<bool>>>]) -> Option<Vec<bool>> {
    let readable: Vec<&Vec<Vec<bool>>> = samples.iter().flatten().collect();
    let first = readable.first()?;
    if readable.len() < 2 {
        return None;
    }
    Some(
        first
            .iter()
            .enumerate()
            .map(|(n, rules)| {
                (0..rules.len()).any(|r| {
                    let votes = readable.iter().filter(|sample| sample[n][r]).count();
                    votes * 2 > readable.len()
                })
            })
            .collect(),
    )
}

#[cfg(test)]
#[path = "rule_scope_tests.rs"]
mod tests;
