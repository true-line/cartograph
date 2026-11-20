//! The section composer's request and reply, as pure functions (#1991,
//! AC-0862).
//!
//! `section-composer@1` drafts one catalogue section in one bounded call. It
//! is handed the section as the export compiles it and the facts that
//! section cites, so it reads the knowledge graph's arrangement of the
//! system (capabilities, their operations, rules and flows) instead of
//! searching for it with a tool budget. It calls no tool.
//!
//! **The request.** [`action`] writes the one completion action: fixed
//! instructions, the section text, one row per citable fact and a response
//! schema whose citation ids are the citable ids alone. The host's bounded
//! call path applies the egress policy, redaction, consent and limits to it
//! unchanged.
//!
//! **The reply.** [`decode`] reads the reply into a [`SectionDraftFinish`].
//! A reply that is not the schema's object refuses with a closed reason. A
//! claim that breaks a rule the admission rule checks per claim (a blank,
//! oversized, non-plain or secret-bearing statement, no citation, too many,
//! an id that is not citable, a proposed design) is left out and counted,
//! rather than refusing every other claim with it. A line break or tab in a
//! statement is read as a space first: it is layout, not content. The
//! caller then applies [`crate::section_draft::admit`] and the neutrality
//! rule as for any draft.

use crate::section_draft::{
    Claim, ClaimKind, MAX_CITATIONS, MAX_CLAIMS, SectionAddress, SectionDraftFinish,
    statement_admissible,
};
use llm::{AnalysisTier, CompletionAction, CompletionPayload};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt::Write as _;

/// The author role of a composed draft.
pub const SECTION_COMPOSER_ROLE: &str = "section-composer@1";

/// Most claims a composer is asked for.
pub const COMPOSER_CLAIMS: usize = 16;

const SYSTEM: &str = "You are Cartograph's section composer. You write one section of a \
product specification for a legacy system, so that a team that has never seen the code \
could rebuild the behaviour from it. You are given the section as Cartograph compiled it \
from the system's knowledge graph, and one row per fact you may cite. Write dense, \
concrete statements in domain terms: what the system offers, to whom, under which \
conditions, with which limits, orderings, statuses and messages. Group related facts \
into one statement; do not restate each table row. Every statement rests only on the \
facts it cites. Never write a file path, a file name or a code identifier (a function, \
class, module or variable name): those stay in the citations. URL routes, HTTP methods \
and statuses, field names and user-facing messages are part of the observable contract \
and may be stated. Choose each claim's kind honestly: implemented_behavior for what the \
code does, documented_intent only for what the target's own documentation or test \
scenario names say is intended, inferred_interpretation for a reading across facts. \
Reply with the JSON object the response schema describes and nothing else.";

/// One fact the composer may cite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposeFact {
    /// Its ADR-0058 citation id.
    pub citation_id: String,
    /// Node or edge label.
    pub label: String,
    /// A display name that is not a source identifier, if any.
    pub name: Option<String>,
    /// Producing tier.
    pub tier: String,
    /// Confidence tier.
    pub confidence: String,
}

/// A derived fact the section prints, over the citable facts it rests on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposeGroup {
    /// The derived fact's citation id; not citable.
    pub citation_id: String,
    /// Its label.
    pub label: String,
    /// Its name.
    pub name: String,
    /// Citable ids it rests on.
    pub rests_on: Vec<String>,
}

/// Everything one composition is written from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeInput<'a> {
    /// The address drafted.
    pub address: &'a SectionAddress,
    /// The section's catalogue title.
    pub title: &'a str,
    /// The section as the export compiles it.
    pub section: &'a str,
    /// The citable facts.
    pub facts: &'a [ComposeFact],
    /// The derived facts the section prints.
    pub groups: &'a [ComposeGroup],
}

/// The fingerprint of this role's definition: its instructions and its
/// schema template.
pub fn definition_fingerprint() -> String {
    let schema = response_schema(&BTreeSet::new());
    crate::source_basis::domain_hash(
        b"cartograph:section-composer-definition:v1\0",
        format!("{SECTION_COMPOSER_ROLE}\0{SYSTEM}\0{schema}").as_bytes(),
    )
}

/// The response schema: up to [`COMPOSER_CLAIMS`] claims, each citing
/// citable ids only. At least one claim and one citation per claim is part
/// of the schema itself, so constrained decoding cannot emit an uncited
/// placeholder (hosted structured output keeps `minItems` 0 and 1).
pub fn response_schema(citable: &BTreeSet<String>) -> String {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["claims"],
        "properties": {
            "claims": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["statement", "claim_kind", "citation_ids"],
                    "properties": {
                        "statement": {"type": "string"},
                        "claim_kind": {"enum": [
                            "implemented_behavior",
                            "documented_intent",
                            "inferred_interpretation"
                        ]},
                        "citation_ids": {
                            "type": "array",
                            "minItems": 1,
                            "items": {"enum": citable.iter().collect::<Vec<_>>()}
                        }
                    }
                }
            }
        }
    })
    .to_string()
}

fn prompt(input: &ComposeInput<'_>) -> String {
    let address = input.address;
    let mut out = String::new();
    let _ = write!(
        out,
        "Compose section {} \"{}\" of document {} (catalogue {}) for scope {}.\n\
         Write at most {COMPOSER_CLAIMS} claims. Each claim cites 1 to {MAX_CITATIONS} \
         citation ids, the most direct facts for it, chosen only from the citable facts \
         below.\n\n<section>\n{}\n</section>\n\n<citable_facts>\n\
         citation_id | label | name | tier | confidence\n",
        address.section,
        input.title,
        address.document,
        address.catalogue_version,
        address.scope,
        input.section.trim_end(),
    );
    for fact in input.facts {
        let _ = writeln!(
            out,
            "{} | {} | {} | {} | {}",
            fact.citation_id,
            fact.label,
            fact.name.as_deref().unwrap_or("-"),
            fact.tier,
            fact.confidence
        );
    }
    out.push_str("</citable_facts>\n");
    if !input.groups.is_empty() {
        out.push_str(
            "\n<derived_facts>\nThese ids are printed in the section but are derived, not \
             citable. Cite the facts each rests on instead.\n",
        );
        for group in input.groups {
            let _ = writeln!(
                out,
                "{} {} \"{}\" rests on: {}",
                group.label,
                group.citation_id,
                group.name,
                group.rests_on.join(", ")
            );
        }
        out.push_str("</derived_facts>\n");
    }
    out
}

/// The one completion action of a composition.
pub fn action(input: &ComposeInput<'_>, action_id: String) -> CompletionAction {
    let citable: BTreeSet<String> = input
        .facts
        .iter()
        .map(|fact| fact.citation_id.clone())
        .collect();
    CompletionAction {
        action_id,
        tier: AnalysisTier::Agentic,
        payload: CompletionPayload {
            response_schema: Some(response_schema(&citable)),
            system: SYSTEM.to_owned(),
            prompt: prompt(input),
            spans: Vec::new(),
        },
    }
}

/// Why a reply yields no finish. Closed: never text from the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposeRefusal {
    /// The reply is not the schema's JSON object.
    NotTheSchema,
    /// No claim is left.
    NoClaims,
}

impl ComposeRefusal {
    /// The reason code.
    pub fn code(self) -> &'static str {
        match self {
            Self::NotTheSchema => "reply_not_the_schema",
            Self::NoClaims => "no_admissible_claim",
        }
    }
}

/// A decoded reply: the finish and how many claims were left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composed {
    /// The claims that keep every per-claim bound.
    pub finish: SectionDraftFinish,
    /// Claims left out for a per-claim bound.
    pub left_out: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    claims: Vec<ReplyClaim>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyClaim {
    statement: String,
    claim_kind: String,
    citation_ids: Vec<String>,
}

/// Decode `text`, the reply to [`action`] for `address`, keeping only ids in
/// `citable`.
pub fn decode(
    address: &SectionAddress,
    text: &str,
    citable: &BTreeSet<String>,
) -> Result<Composed, ComposeRefusal> {
    let reply: Reply =
        serde_json::from_str(text.trim()).map_err(|_| ComposeRefusal::NotTheSchema)?;
    let mut claims = Vec::new();
    let mut left_out = 0;
    for claim in reply.claims {
        let kind = match claim.claim_kind.as_str() {
            "implemented_behavior" => Some(ClaimKind::Implemented),
            "documented_intent" => Some(ClaimKind::DocumentedIntent),
            "inferred_interpretation" => Some(ClaimKind::Interpretation),
            _ => None,
        };
        let mut citations: Vec<String> = Vec::new();
        for id in claim.citation_ids {
            if !citations.contains(&id) {
                citations.push(id);
            }
        }
        // Layout whitespace is not content; any other control character is.
        let statement: String = claim
            .statement
            .chars()
            .map(|c| {
                if matches!(c, '\n' | '\r' | '\t') {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        let statement = statement.trim().to_owned();
        let cites_ok = !citations.is_empty()
            && citations.len() <= MAX_CITATIONS
            && citations.iter().all(|id| citable.contains(id));
        match kind {
            Some(kind)
                if cites_ok && statement_admissible(&statement) && claims.len() < MAX_CLAIMS =>
            {
                claims.push(Claim {
                    statement,
                    kind,
                    citations,
                })
            }
            _ => left_out += 1,
        }
    }
    if claims.is_empty() {
        return Err(ComposeRefusal::NoClaims);
    }
    Ok(Composed {
        finish: SectionDraftFinish {
            address: address.clone(),
            claims,
        },
        left_out,
    })
}

#[cfg(test)]
#[path = "section_compose_tests.rs"]
mod tests;
