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
//! an id that is not citable, a proposed design), or whose statement has
//! fewer than [`MIN_STATEMENT_WORDS`] words, is left out and counted,
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

/// Fewest words a composed statement has. A composer is asked for dense
/// statements; a shorter one ("placeholder", "TBD") states nothing.
pub const MIN_STATEMENT_WORDS: usize = 5;

/// Whether `statement` has at least [`MIN_STATEMENT_WORDS`] words.
fn substantive(statement: &str) -> bool {
    statement
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .count()
        >= MIN_STATEMENT_WORDS
}

pub(crate) const SYSTEM: &str = "You are Cartograph's section composer. You write one section of a \
product specification for a legacy system, so that a team that has never seen the code \
could rebuild the behaviour from it. You are given the section as Cartograph compiled it \
from the system's knowledge graph, and one row per fact you may cite. Write dense, \
concrete statements in domain terms: what the system offers, to whom, under which \
conditions, with which limits, orderings, statuses and messages. Group related facts \
into one statement; do not restate each table row. Every statement rests only on the \
facts it cites. Never write a file path, a file name or a code identifier (a function, \
class, module or variable name): those stay in the citations. URL routes, HTTP methods \
and statuses, field names and user-facing messages are part of the observable contract \
and may be stated, but a route you state must be one a fact you cite shows: cite its \
endpoint. A number, count or version you state must appear in a fact you cite, and a \
statement about endpoints or routes cites an Endpoint fact. Choose each claim's kind honestly: implemented_behavior for what the \
code does, documented_intent only for what the target's own documentation or test \
scenario names say is intended, inferred_interpretation for a reading across facts. \
Reply with the JSON object the response schema describes and nothing else.";

/// How each D1 section is written, beyond [`SYSTEM`]: what a reader of
/// that section needs from it. A section without an entry gets none.
const GUIDANCE: &[(&str, &str)] = &[
    (
        "1.1",
        "State the system's purpose and scope from the sections it summarises: what the \
         system is for, who uses it, its main capability areas and what lies outside it. \
         Write a short overview, not a capability-by-capability listing, and never name \
         a route, callable or file.",
    ),
    (
        "1.2",
        "Describe each actor or role by what it is, what it may do and what it may not.",
    ),
    (
        "1.3",
        "Describe the system's capabilities as a product owner would, area by area. When \
         the section lists capability areas, follow them: describe each area it prints \
         rather than inventing another grouping, and in prose you may merge or rename an \
         area whose name is weak or misleading. Otherwise group the capabilities into \
         areas yourself. For each area state its purpose, who uses it (an end user, an \
         API client, an administrator, a developer, an external provider) and the outcome \
         it delivers, including notable conditions and limits. A route is evidence, not \
         content: name at most two representative routes in a statement and never \
         enumerate routes, methods or versions.",
    ),
    (
        "1.4",
        "State each acceptance criterion as a condition and its expected outcome, grouped \
         by capability. Prefer the criteria that constrain behaviour (refusals, limits, \
         security guarantees) over the ones that only say an operation works.",
    ),
    (
        "1.5",
        "This section lists rules as code: where (a callable), when (branch conditions) \
         and outcome (a raise or an error response), grouped under the capability whose \
         handlers reach them. Translate each into domain terms: who is refused, under \
         which condition, with which user-facing message and status. Quote messages and \
         statuses exactly; never repeat a callable, variable or class name. Do not state \
         an order between checks unless the facts establish it.",
    ),
];

/// The guidance for D1 section `number`, if it has any.
pub fn guidance(number: &str) -> Option<&'static str> {
    GUIDANCE
        .iter()
        .find(|(section, _)| *section == number)
        .map(|(_, text)| *text)
}

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
    /// The derived fact's citation id; it is also a citable fact (#2040).
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

/// The fingerprint of this role's definition: its instructions, its
/// per-section guidance and its schema template.
pub fn definition_fingerprint() -> String {
    let schema = response_schema(&BTreeSet::new());
    crate::source_basis::domain_hash(
        b"cartograph:section-composer-definition:v1\0",
        format!("{SECTION_COMPOSER_ROLE}\0{SYSTEM}\0{GUIDANCE:?}\0{schema}").as_bytes(),
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
    prompt_with(input, COMPOSER_CLAIMS)
}

/// The composer's prompt asking for at most `max_claims` claims. Shared with
/// the gated workflow (#2083), which composes one bounded chunk per call.
pub(crate) fn prompt_with(input: &ComposeInput<'_>, max_claims: usize) -> String {
    let address = input.address;
    let mut out = String::new();
    let _ = write!(
        out,
        "Compose section {} \"{}\" of document {} (catalogue {}) for scope {}.\n\
         Write at most {max_claims} claims. Each claim cites 1 to {MAX_CITATIONS} \
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
    if let Some(text) = guidance(&address.section) {
        // Before the facts, so it frames how they are read.
        let at = out.find("<section>").unwrap_or(out.len());
        out.insert_str(at, &format!("<guidance>\n{text}\n</guidance>\n\n"));
    }
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
            "\n<derived_facts>\nThese ids are printed in the section and derived from the \
             facts each rests on. Cite a derived id for what it names, and the facts it rests \
             on for the detail.\n",
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
    /// How many claims each bound left out, by reason (#2349).
    pub reasons: std::collections::BTreeMap<&'static str, usize>,
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
    let mut reasons = std::collections::BTreeMap::new();
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
        // The first bound a claim fails names why it is left out.
        let reason = match kind {
            None => Some("claim_kind_unknown"),
            Some(_) if !cites_ok => Some("citation_not_citable"),
            Some(_) if !statement_admissible(&statement) => Some("statement_not_admissible"),
            Some(_) if !substantive(&statement) => Some("statement_not_substantive"),
            Some(_) if claims.len() >= MAX_CLAIMS => Some("too_many_claims"),
            Some(_) => None,
        };
        match (kind, reason) {
            (Some(kind), None) => claims.push(Claim {
                statement,
                kind,
                citations,
            }),
            (_, reason) => {
                left_out += 1;
                *reasons
                    .entry(reason.unwrap_or("claim_kind_unknown"))
                    .or_default() += 1;
            }
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
        reasons,
    })
}

#[cfg(test)]
#[path = "section_compose_tests.rs"]
mod tests;
