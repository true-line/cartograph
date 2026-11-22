//! The claim reviewer's request and reply, as pure functions (#2419).
//!
//! `claim-reviewer@1` reads the claims a composition produced beside the
//! text of the facts each one cites, in one bounded call, and says of each
//! whether its facts show it. It catches what a lexical check cannot: a
//! rule stated for a wider scope than the capability it belongs to, detail
//! resting on a fact whose name does not show it, a misread condition or
//! actor. It calls no tool and writes nothing; the host leaves out each
//! claim it judges an overreach.

use llm::{AnalysisTier, CompletionAction, CompletionPayload};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The role.
pub const CLAIM_REVIEWER_ROLE: &str = "claim-reviewer@1";

/// Most characters of one fact's text the reviewer reads.
pub const FACT_TEXT_CHARS: usize = 400;

const SYSTEM: &str = "You are Cartograph's claim reviewer. You check claims of a product \
specification against the facts they cite, one verdict per claim. A claim is shown when \
every assertion in it is shown by one of its cited facts. It is partly shown when small \
details are not shown but nothing it says is wrong or wider than its facts. It is an \
overreach when it says something its cited facts do not show or contradict: a rule stated \
for a wider scope than the capability it is listed under (other capabilities, a whole area, \
every call, form or provider), a list of jobs, actions, screens or steps that no cited \
fact's name or text names, a condition, status or message other than the rule's, or the \
wrong actor. Judge only from the facts given, never from what such a system usually does.";

/// One claim to review: its statement and the ids it cites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewClaim<'a> {
    /// The claim's statement.
    pub statement: &'a str,
    /// The citation ids it cites.
    pub citations: &'a [String],
}

/// A verdict on one claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every assertion is shown by a cited fact.
    Shown,
    /// Small details are not shown; nothing is wrong or wider.
    Partly,
    /// It says something its cited facts do not show or contradict.
    Overreach,
}

/// The response schema: exactly `claims` verdicts, one per claim in order.
pub fn response_schema(claims: usize) -> String {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["verdicts"],
        "properties": {
            "verdicts": {
                "type": "array",
                "minItems": claims,
                "maxItems": claims,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["claim", "verdict"],
                    "properties": {
                        "claim": {"type": "integer", "minimum": 1, "maximum": claims},
                        "verdict": {"enum": ["shown", "partly", "overreach"]}
                    }
                }
            }
        }
    })
    .to_string()
}

/// The fingerprint of this role's definition: its instructions and its
/// schema template.
pub fn definition_fingerprint() -> String {
    crate::source_basis::domain_hash(
        b"cartograph:claim-reviewer-definition:v1\0",
        format!("{CLAIM_REVIEWER_ROLE}\0{SYSTEM}\0{}", response_schema(0)).as_bytes(),
    )
}

/// The one completion action reviewing `claims`, with `facts` the text of
/// each cited fact by citation id. Each fact is printed once, cut to
/// [`FACT_TEXT_CHARS`] characters; a cited id with no text is printed as
/// such.
pub fn action(
    claims: &[ReviewClaim<'_>],
    facts: &BTreeMap<String, String>,
    action_id: String,
) -> CompletionAction {
    let mut prompt = String::from("<facts>\n");
    let cited: std::collections::BTreeSet<&String> =
        claims.iter().flat_map(|claim| claim.citations).collect();
    for id in cited {
        let text = facts.get(id).map_or("(no text)", String::as_str);
        let cut: String = text.chars().take(FACT_TEXT_CHARS).collect();
        let _ = writeln!(prompt, "{id}: {}", cut.replace('\n', " "));
    }
    prompt.push_str("</facts>\n\n<claims>\n");
    for (n, claim) in claims.iter().enumerate() {
        let _ = writeln!(
            prompt,
            "{}. {} [cites: {}]",
            n + 1,
            claim.statement.replace('\n', " "),
            claim.citations.join(", ")
        );
    }
    let _ = write!(
        prompt,
        "</claims>\n\nGive one verdict for each of the {} claims, numbered as above.",
        claims.len()
    );
    CompletionAction {
        action_id,
        tier: AnalysisTier::Agentic,
        payload: CompletionPayload {
            response_schema: Some(response_schema(claims.len())),
            system: SYSTEM.to_owned(),
            prompt,
            spans: Vec::new(),
        },
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    verdicts: Vec<ReplyVerdict>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyVerdict {
    claim: usize,
    verdict: Verdict,
}

/// The verdict on each of `claims` claims, in order, or `None` when the
/// reply is not the schema's object or does not give exactly one verdict
/// for each claim.
pub fn decode(text: &str, claims: usize) -> Option<Vec<Verdict>> {
    let reply: Reply = serde_json::from_str(text.trim()).ok()?;
    let mut verdicts = vec![None; claims];
    for given in reply.verdicts {
        let slot = verdicts.get_mut(given.claim.checked_sub(1)?)?;
        if slot.replace(given.verdict).is_some() {
            return None;
        }
    }
    verdicts.into_iter().collect()
}

#[cfg(test)]
#[path = "claim_review_tests.rs"]
mod tests;
