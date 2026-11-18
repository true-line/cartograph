//! The drafting run's request and result, as pure functions (#1748, SPEC-15
//! slice 6, ADR-0064 decisions 3 and 4, AC-0668).
//!
//! A drafting run is an ordinary investigation task for the role
//! `section-writer@1`: it runs inside the existing limits, consent,
//! cancellation, replay and privacy checks, and nothing here changes them.
//! This module holds the two things the role adds.
//!
//! **The request.** [`question`] writes the one question a drafting run may
//! be started with, and [`requested_address`] reads the address back from it.
//! The address therefore lives in the saved request and needs no second
//! record.
//!
//! **The result.** [`finish_of`] turns the findings of a completed run into
//! a [`SectionDraftFinish`]: one claim per finding, in order, citing the
//! ADR-0058 id of each fact the finding's admitted citations rest on. The
//! caller then applies [`crate::section_draft::admit`] against the citation
//! set of the address. A finding the role may not return is refused with a
//! closed reason and yields no draft.

use crate::investigation::{InvestigationCitation, InvestigationClaimKind, InvestigationFinding};
use crate::section_draft::{Claim, ClaimKind, SYSTEM_SCOPE, SectionAddress, SectionDraftFinish};
use crate::source_basis::TaskFactKey;
use std::collections::BTreeMap;

/// The author role of a section draft.
pub const SECTION_WRITER_ROLE: &str = "section-writer@1";

const MAX_ADDRESS_PART_BYTES: usize = 64;
const MAX_TITLE_BYTES: usize = 160;

/// The question of a drafting run for `address`, whose catalogue title is
/// `title`.
pub fn question(address: &SectionAddress, title: &str) -> String {
    format!(
        "Draft section {} \"{}\" of document {} (catalogue {}) for scope {}.",
        address.section, title, address.document, address.catalogue_version, address.scope
    )
}

/// The address and title a drafting run's question names, or none when the
/// question is not exactly what [`question`] writes.
pub fn requested_address(text: &str) -> Option<(SectionAddress, &str)> {
    let rest = text.strip_prefix("Draft section ")?;
    let (section, rest) = rest.split_once(" \"")?;
    let (title, rest) = rest.rsplit_once("\" of document ")?;
    let (document, rest) = rest.split_once(" (catalogue ")?;
    let (catalogue_version, rest) = rest.split_once(") for scope ")?;
    let scope = rest.strip_suffix('.')?;
    let part = |value: &str| {
        !value.is_empty()
            && value.len() <= MAX_ADDRESS_PART_BYTES
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
    };
    if !(part(section) && part(document) && part(catalogue_version) && valid_scope(scope))
        || title.is_empty()
        || title.len() > MAX_TITLE_BYTES
        || title.chars().any(char::is_control)
    {
        return None;
    }
    let address = SectionAddress {
        catalogue_version: catalogue_version.to_owned(),
        document: document.to_owned(),
        section: section.to_owned(),
        scope: scope.to_owned(),
    };
    // One spelling per address: what `question` writes, and nothing else.
    (question(&address, title) == text).then_some((address, title))
}

/// Whether `scope` is the system scope or is shaped like a citation id.
pub fn valid_scope(scope: &str) -> bool {
    scope == SYSTEM_SCOPE
        || scope.strip_prefix("F-").is_some_and(|hex| {
            hex.len() == 16
                && hex
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
        })
}

/// Why a completed run's findings are not a draft finish. Closed: a code
/// and a claim position, never text from the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishRefusal {
    /// A finding proposes a design; a section describes the system as it is.
    ProposedDesign {
        /// Zero-based position of the finding.
        claim: usize,
    },
    /// A finding cites an id the run's ledger does not hold.
    UnknownCitation {
        /// Zero-based position of the finding.
        claim: usize,
    },
}

impl FinishRefusal {
    /// The reason code.
    pub fn code(self) -> &'static str {
        match self {
            Self::ProposedDesign { .. } => "proposed_design",
            Self::UnknownCitation { .. } => "unknown_citation",
        }
    }
}

/// The draft finish a completed run's `findings` make for `address`.
///
/// `citations` is the run's admitted ledger and `fact_id` gives the
/// ADR-0058 citation id of a fact. A claim cites each fact once, in the
/// order its finding first cites it: two source spans of one fact are one
/// citation.
pub fn finish_of(
    address: &SectionAddress,
    findings: &[InvestigationFinding],
    citations: &[InvestigationCitation],
    fact_id: impl Fn(&TaskFactKey) -> String,
) -> Result<SectionDraftFinish, FinishRefusal> {
    let ledger: BTreeMap<&str, &TaskFactKey> = citations
        .iter()
        .map(|citation| (citation.citation_id.as_str(), &citation.fact))
        .collect();
    let mut claims = Vec::with_capacity(findings.len());
    for (claim, finding) in findings.iter().enumerate() {
        let kind = match finding.claim_kind {
            InvestigationClaimKind::ImplementedBehavior => ClaimKind::Implemented,
            InvestigationClaimKind::DocumentedIntent => ClaimKind::DocumentedIntent,
            InvestigationClaimKind::InferredInterpretation => ClaimKind::Interpretation,
            InvestigationClaimKind::ProposedDesign => {
                return Err(FinishRefusal::ProposedDesign { claim });
            }
        };
        let mut cited: Vec<String> = Vec::with_capacity(finding.citation_ids.len());
        for id in &finding.citation_ids {
            let fact = ledger
                .get(id.as_str())
                .ok_or(FinishRefusal::UnknownCitation { claim })?;
            let id = fact_id(fact);
            if !cited.contains(&id) {
                cited.push(id);
            }
        }
        claims.push(Claim {
            statement: finding.statement.clone(),
            kind,
            citations: cited,
        });
    }
    Ok(SectionDraftFinish {
        address: address.clone(),
        claims,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::investigation::InvestigationEvidenceOrigin;
    use core_prov::{ConfidenceTier, Tier};

    fn address(scope: &str) -> SectionAddress {
        SectionAddress {
            catalogue_version: "1.0".into(),
            document: "D1".into(),
            section: "1.3".into(),
            scope: scope.into(),
        }
    }

    fn finding(kind: InvestigationClaimKind, cites: &[&str]) -> InvestigationFinding {
        InvestigationFinding {
            finding_id: "finding-1".into(),
            claim_kind: kind,
            title: "Title".into(),
            statement: "The service signs a user in.".into(),
            citation_ids: cites.iter().map(|id| (*id).to_owned()).collect(),
            limitations: Vec::new(),
            tier: Tier::Agentic,
            confidence_tier: ConfidenceTier::InferredWeak,
        }
    }

    fn citation(id: &str, node: &str) -> InvestigationCitation {
        InvestigationCitation {
            citation_id: id.into(),
            fact: TaskFactKey::Node { id: node.into() },
            fact_digest: String::new(),
            source: None,
            role: None,
            index: None,
            text_hash: None,
            origin: InvestigationEvidenceOrigin::GraphMetadata,
        }
    }

    fn fact_id(fact: &TaskFactKey) -> String {
        match fact {
            TaskFactKey::Node { id } => format!("F-{id}"),
            TaskFactKey::Edge { .. } => "F-edge".into(),
        }
    }

    // AC-0668: the address is carried by the question, in one spelling.
    #[test]
    fn question_round_trips_and_no_other_spelling_is_an_address() {
        for scope in [SYSTEM_SCOPE, "F-0123456789abcdef"] {
            let address = address(scope);
            let text = question(&address, "Capabilities");
            assert_eq!(
                requested_address(&text),
                Some((address.clone(), "Capabilities"))
            );
        }
        let text = question(&address(SYSTEM_SCOPE), "A \"quoted\" title");
        assert_eq!(
            requested_address(&text).map(|(_, title)| title),
            Some("A \"quoted\" title")
        );
        for bad in [
            "What does the service do?",
            "Draft section 1.3 \"Capabilities\" of document D1 (catalogue 1.0) for scope system",
            "Draft section 1.3 \"Capabilities\" of document D1 (catalogue 1.0) for scope system. More.",
            "Draft section 1.3 \"Capabilities\" of document D1 (catalogue 1.0) for scope everything.",
            "Draft section 1.3 \"Capabilities\" of document D1 (catalogue 1.0) for scope F-0123456789ABCDEF.",
            "Draft section 1 3 \"Capabilities\" of document D1 (catalogue 1.0) for scope system.",
            "Draft section 1.3 \"\" of document D1 (catalogue 1.0) for scope system.",
            " Draft section 1.3 \"Capabilities\" of document D1 (catalogue 1.0) for scope system.",
        ] {
            assert_eq!(requested_address(bad), None, "{bad}");
        }
    }

    // AC-0668: one claim per finding, in order, citing facts by citation id.
    #[test]
    fn findings_become_claims_that_cite_each_fact_once() {
        let ledger = [
            citation("evidence-1", "a"),
            citation("evidence-2", "a"),
            citation("evidence-3", "b"),
        ];
        let findings = [
            finding(
                InvestigationClaimKind::ImplementedBehavior,
                &["evidence-3", "evidence-1", "evidence-2"],
            ),
            finding(InvestigationClaimKind::DocumentedIntent, &["evidence-2"]),
            finding(
                InvestigationClaimKind::InferredInterpretation,
                &["evidence-1"],
            ),
        ];
        let finish = finish_of(&address(SYSTEM_SCOPE), &findings, &ledger, fact_id).unwrap();
        assert_eq!(finish.address, address(SYSTEM_SCOPE));
        let kinds: Vec<_> = finish.claims.iter().map(|claim| claim.kind).collect();
        assert_eq!(
            kinds,
            [
                ClaimKind::Implemented,
                ClaimKind::DocumentedIntent,
                ClaimKind::Interpretation
            ]
        );
        assert_eq!(finish.claims[0].citations, ["F-b", "F-a"]);
        assert_eq!(finish.claims[1].citations, ["F-a"]);
        assert_eq!(finish.claims[0].statement, "The service signs a user in.");
    }

    // AC-0668: a finding the role may not return yields no finish.
    #[test]
    fn proposed_design_and_unknown_citations_are_refused_with_a_closed_code() {
        let ledger = [citation("evidence-1", "a")];
        let refused = finish_of(
            &address(SYSTEM_SCOPE),
            &[
                finding(InvestigationClaimKind::ImplementedBehavior, &["evidence-1"]),
                finding(InvestigationClaimKind::ProposedDesign, &["evidence-1"]),
            ],
            &ledger,
            fact_id,
        )
        .unwrap_err();
        assert_eq!(refused, FinishRefusal::ProposedDesign { claim: 1 });
        assert_eq!(refused.code(), "proposed_design");
        let refused = finish_of(
            &address(SYSTEM_SCOPE),
            &[finding(
                InvestigationClaimKind::ImplementedBehavior,
                &["evidence-9"],
            )],
            &ledger,
            fact_id,
        )
        .unwrap_err();
        assert_eq!(refused, FinishRefusal::UnknownCitation { claim: 0 });
        assert_eq!(refused.code(), "unknown_citation");
    }
}
