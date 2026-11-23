//! The rule-scope check's request and reply, as pure functions (#2482).
//!
//! `rule-scope@1` reads the claims that cite business rules beside the facts
//! they cite and says, for each rule a claim cites, which of the claim's
//! cited capabilities the claim states that rule for, or `all`. It extracts
//! rather than judges: the host compares each answer with the capabilities
//! the rule is listed under, and a claim stating a rule wider than them (a
//! sign-in error of one provider stated for every provider, an API refusal
//! stated for the dashboard too) is left out. It calls no tool and writes
//! nothing.

use crate::claim_review::FACT_TEXT_CHARS;
use llm::{AnalysisTier, CompletionAction, CompletionPayload};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// The role's name and version.
pub const RULE_SCOPE_ROLE: &str = "rule-scope@1";

/// The answer for a rule stated beyond the claim's cited capabilities.
pub const ALL: &str = "all";

/// How many times the same request is asked; a rule is read as widened on a
/// majority of the readable replies.
pub const SCOPE_SAMPLES: usize = 3;

const SYSTEM: &str = "You read claims of a product specification beside the facts they \
cite. Some cited facts are business rules, each listed under the capability it belongs to. \
For each rule a claim cites, say which of the claim's cited capabilities the claim states \
that rule for, by their ids: only those the claim says the rule's condition or outcome holds \
for, not every capability named in the same sentence. Answer \"all\" when the claim states \
the rule for a whole area, for every provider, form, page or call, or for operations beyond \
those capabilities. Report only what the claim says, never what the rule says.";

/// What one claim states each of its rules for: the capability ids (or
/// [`ALL`]) by rule.
pub type Scopes = BTreeMap<String, Vec<String>>;

/// One claim to check: its statement, every id it cites, the excerpt rules
/// among them and the Capability facts among them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeClaim<'a> {
    /// The claim's statement.
    pub statement: &'a str,
    /// Every citation id of the claim.
    pub citations: &'a [String],
    /// The excerpt rules it cites.
    pub rules: Vec<&'a str>,
    /// The Capability facts it cites.
    pub capabilities: Vec<&'a str>,
}

/// The response schema: exactly one scope for each rule each claim cites,
/// a claim's rule and answer drawn from its own citations.
pub fn response_schema(claims: &[ScopeClaim<'_>]) -> String {
    let pairs: usize = claims.iter().map(|claim| claim.rules.len()).sum();
    let branches: Vec<serde_json::Value> = claims
        .iter()
        .enumerate()
        .map(|(n, claim)| {
            let mut answers: Vec<&str> = claim.capabilities.clone();
            answers.push(ALL);
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["claim", "rule", "applies_to"],
                "properties": {
                    "claim": {"enum": [n + 1]},
                    "rule": {"enum": claim.rules},
                    "applies_to": {"type": "array", "minItems": 1, "items": {"enum": answers}}
                }
            })
        })
        .collect();
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["scopes"],
        "properties": {
            "scopes": {
                "type": "array",
                "minItems": pairs,
                "maxItems": pairs,
                "items": {"anyOf": branches}
            }
        }
    })
    .to_string()
}

/// The fingerprint of this role's definition: its instructions and the
/// version of its schema.
pub fn definition_fingerprint() -> String {
    crate::source_basis::domain_hash(
        b"cartograph:rule-scope-definition:v1\0",
        format!("{RULE_SCOPE_ROLE}\0{SYSTEM}\0{ALL}").as_bytes(),
    )
}

/// The one completion action checking `claims`, with `facts` the text of
/// each cited fact by citation id, printed once and cut like the
/// reviewer's.
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
    prompt.push_str("</facts>\n\n<claims>\n");
    for (n, claim) in claims.iter().enumerate() {
        let _ = writeln!(
            prompt,
            "{}. {} [cites: {}] [rules: {}]",
            n + 1,
            claim.statement.replace('\n', " "),
            claim.citations.join(", "),
            claim.rules.join(", ")
        );
    }
    let pairs: usize = claims.iter().map(|claim| claim.rules.len()).sum();
    let _ = write!(
        prompt,
        "</claims>\n\nGive one scope for each rule each claim cites: {pairs} in all."
    );
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
    scopes: Vec<ReplyScope>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyScope {
    claim: usize,
    rule: String,
    applies_to: Vec<String>,
}

/// What each claim states each of its rules for, by claim then rule, or
/// `None` when the reply is not the schema's object or does not give
/// exactly one scope, drawn from the claim's own citations, for each rule
/// each claim cites.
pub fn decode(text: &str, claims: &[ScopeClaim<'_>]) -> Option<Vec<Scopes>> {
    let reply: Reply = serde_json::from_str(text.trim()).ok()?;
    let mut scopes = vec![BTreeMap::new(); claims.len()];
    for given in reply.scopes {
        let claim = claims.get(given.claim.checked_sub(1)?)?;
        let fits = claim.rules.contains(&given.rule.as_str())
            && !given.applies_to.is_empty()
            && given
                .applies_to
                .iter()
                .all(|id| id == ALL || claim.capabilities.contains(&id.as_str()));
        let slot = &mut scopes[given.claim - 1];
        if !fits || slot.insert(given.rule, given.applies_to).is_some() {
            return None;
        }
    }
    claims
        .iter()
        .zip(&scopes)
        .all(|(claim, scope)| scope.len() == claim.rules.len())
        .then_some(scopes)
}

/// Whether `applies_to` states a rule wider than `in_scope`, the
/// capabilities the rule holds for: `all`, or any capability outside them.
pub fn widened(applies_to: &[String], in_scope: &BTreeSet<&str>) -> bool {
    applies_to
        .iter()
        .any(|id| id == ALL || !in_scope.contains(id.as_str()))
}

/// Which claims state a rule wider than it holds, on a majority of the
/// readable `samples` (at least two): a claim is flagged when any of its
/// rules is read as widened by more than half of them. `in_scope` gives,
/// by claim then rule, the capabilities the rule holds for. `None` when
/// fewer than two samples could be read.
pub fn widened_claims(
    samples: &[Option<Vec<Scopes>>],
    in_scope: &[BTreeMap<&str, BTreeSet<&str>>],
) -> Option<Vec<bool>> {
    let readable: Vec<&Vec<Scopes>> = samples
        .iter()
        .flatten()
        .filter(|scopes| scopes.len() == in_scope.len())
        .collect();
    if readable.len() < 2 {
        return None;
    }
    Some(
        in_scope
            .iter()
            .enumerate()
            .map(|(n, rules)| {
                rules.iter().any(|(rule, holds_for)| {
                    let votes = readable
                        .iter()
                        .filter(|scopes| {
                            scopes[n]
                                .get(*rule)
                                .is_some_and(|applies| widened(applies, holds_for))
                        })
                        .count();
                    votes * 2 > readable.len()
                })
            })
            .collect(),
    )
}

#[cfg(test)]
#[path = "rule_scope_tests.rs"]
mod tests;
