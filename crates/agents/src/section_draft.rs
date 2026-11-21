//! Section drafts and their admission (#1748, SPEC-15 slice 2, ADR-0064
//! decisions 1 to 4, AC-0664).
//!
//! A draft is an ordered list of claims for one section address, and nothing
//! else. Each claim is one statement, its kind and the citation ids it rests
//! on. [`admit`] is the whole accept-or-refuse rule for a drafting run's
//! finish: it is pure, reads no store and calls no model. A refused finish
//! yields one closed reason code and no draft.
//!
//! **What is checked.** The finish targets the requested address. It holds 1
//! to [`MAX_CLAIMS`] claims. Each statement is non-blank, at most
//! [`MAX_STATEMENT_BYTES`] bytes, free of control characters and of
//! recognized secrets. Each claim cites 1 to [`MAX_CITATIONS`] distinct ids,
//! every one in the run's citation set.
//!
//! **What is not checked.** Whether the cited facts support the statement.
//! Citation membership is not entailment (ADR-0064 decision 12): that is
//! for human review and content acceptance.
//!
//! **Recorded hashes.** An admitted draft carries, for each distinct id it
//! cites, the content hash that fact had when the run read it, so a later
//! read can tell that the fact changed (ADR-0064 decision 8).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Most claims one draft holds.
pub const MAX_CLAIMS: usize = 256;
/// Most UTF-8 bytes one statement holds.
pub const MAX_STATEMENT_BYTES: usize = 2_048;
/// Most citation ids one claim holds.
pub const MAX_CITATIONS: usize = 16;
/// The scope of a section written once for the whole system.
pub const SYSTEM_SCOPE: &str = "system";

/// One section of one catalogue document, for one subject or the system.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionAddress {
    /// The catalogue version the section table names.
    pub catalogue_version: String,
    /// `D1`..`D11`.
    pub document: String,
    /// The catalogue number, such as `1.3`.
    pub section: String,
    /// [`SYSTEM_SCOPE`], or the citation id of the subject.
    pub scope: String,
}

/// What a claim asserts about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    /// What the code does.
    Implemented,
    /// What a target document says is intended.
    DocumentedIntent,
    /// A reading of the cited facts.
    Interpretation,
}

/// One statement with the facts it rests on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    /// The statement, as plain text.
    pub statement: String,
    /// What it asserts about.
    pub kind: ClaimKind,
    /// The ADR-0058 citation ids it rests on, in the order given.
    pub citations: Vec<String>,
}

/// The finish payload of a drafting run, before admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionDraftFinish {
    /// The address the role says it drafted.
    pub address: SectionAddress,
    /// The claims, in reading order.
    pub claims: Vec<Claim>,
}

/// An admitted draft: the finish, and the content hash of every fact it
/// cites as the run read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionDraft {
    /// The address drafted.
    pub address: SectionAddress,
    /// The claims, in reading order.
    pub claims: Vec<Claim>,
    /// Citation id to content hash, for each distinct cited id.
    pub cited_hashes: BTreeMap<String, String>,
}

/// Why a finish was refused. Closed: a refusal carries a code and a claim
/// position, never text from the finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum DraftRefusal {
    /// The finish targets another address than the run was asked for.
    AddressMismatch,
    /// The finish holds no claim.
    NoClaims,
    /// The finish holds more than [`MAX_CLAIMS`] claims.
    TooManyClaims,
    /// A statement is blank.
    BlankStatement {
        /// Zero-based position of the claim.
        claim: usize,
    },
    /// A statement is longer than [`MAX_STATEMENT_BYTES`].
    StatementTooLong {
        /// Zero-based position of the claim.
        claim: usize,
    },
    /// A statement holds a control character other than a space.
    StatementNotPlainText {
        /// Zero-based position of the claim.
        claim: usize,
    },
    /// A statement holds a recognized secret.
    StatementHoldsSecret {
        /// Zero-based position of the claim.
        claim: usize,
    },
    /// A claim cites nothing.
    NoCitation {
        /// Zero-based position of the claim.
        claim: usize,
    },
    /// A claim cites more than [`MAX_CITATIONS`] ids.
    TooManyCitations {
        /// Zero-based position of the claim.
        claim: usize,
    },
    /// A claim cites one id twice.
    DuplicateCitation {
        /// Zero-based position of the claim.
        claim: usize,
    },
    /// A claim cites an id the run was not admitted.
    CitationNotAdmitted {
        /// Zero-based position of the claim.
        claim: usize,
    },
}

impl DraftRefusal {
    /// The reason code, as serialized.
    pub fn code(self) -> &'static str {
        match self {
            Self::AddressMismatch => "address_mismatch",
            Self::NoClaims => "no_claims",
            Self::TooManyClaims => "too_many_claims",
            Self::BlankStatement { .. } => "blank_statement",
            Self::StatementTooLong { .. } => "statement_too_long",
            Self::StatementNotPlainText { .. } => "statement_not_plain_text",
            Self::StatementHoldsSecret { .. } => "statement_holds_secret",
            Self::NoCitation { .. } => "no_citation",
            Self::TooManyCitations { .. } => "too_many_citations",
            Self::DuplicateCitation { .. } => "duplicate_citation",
            Self::CitationNotAdmitted { .. } => "citation_not_admitted",
        }
    }
}

/// Whether `statement` keeps every per-statement rule of [`admit`]: at
/// most [`MAX_STATEMENT_BYTES`], not blank, plain text and free of
/// recognized secrets.
pub fn statement_admissible(statement: &str) -> bool {
    statement.len() <= MAX_STATEMENT_BYTES
        && statement.chars().any(char::is_alphanumeric)
        && !statement.chars().any(char::is_control)
        && core_redact::recognized_secrets(statement).is_empty()
}

/// Admit `finish` as a draft for `requested`, or refuse it whole.
///
/// `citation_set` is the run's admitted facts: citation id to the content
/// hash the run read. The first failing rule, in claim order, is the reason.
pub fn admit(
    requested: &SectionAddress,
    citation_set: &BTreeMap<String, String>,
    finish: SectionDraftFinish,
) -> Result<SectionDraft, DraftRefusal> {
    if finish.address != *requested {
        return Err(DraftRefusal::AddressMismatch);
    }
    if finish.claims.is_empty() {
        return Err(DraftRefusal::NoClaims);
    }
    if finish.claims.len() > MAX_CLAIMS {
        return Err(DraftRefusal::TooManyClaims);
    }
    let mut cited_hashes = BTreeMap::new();
    for (claim, value) in finish.claims.iter().enumerate() {
        let statement = value.statement.as_str();
        // Bounded before anything scans it.
        if statement.len() > MAX_STATEMENT_BYTES {
            return Err(DraftRefusal::StatementTooLong { claim });
        }
        if !statement.chars().any(char::is_alphanumeric) {
            return Err(DraftRefusal::BlankStatement { claim });
        }
        if statement.chars().any(char::is_control) {
            return Err(DraftRefusal::StatementNotPlainText { claim });
        }
        if !core_redact::recognized_secrets(statement).is_empty() {
            return Err(DraftRefusal::StatementHoldsSecret { claim });
        }
        if value.citations.is_empty() {
            return Err(DraftRefusal::NoCitation { claim });
        }
        if value.citations.len() > MAX_CITATIONS {
            return Err(DraftRefusal::TooManyCitations { claim });
        }
        let mut once = BTreeSet::new();
        for id in &value.citations {
            if !once.insert(id.as_str()) {
                return Err(DraftRefusal::DuplicateCitation { claim });
            }
            let Some(hash) = citation_set.get(id) else {
                return Err(DraftRefusal::CitationNotAdmitted { claim });
            };
            cited_hashes.insert(id.clone(), hash.clone());
        }
    }
    Ok(SectionDraft {
        address: finish.address,
        claims: finish.claims,
        cited_hashes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address() -> SectionAddress {
        SectionAddress {
            catalogue_version: "1.0".into(),
            document: "D1".into(),
            section: "1.3".into(),
            scope: SYSTEM_SCOPE.into(),
        }
    }

    fn set() -> BTreeMap<String, String> {
        [
            ("F-0000000000000001", "hash-1"),
            ("F-0000000000000002", "hash-2"),
            ("F-0000000000000003", "hash-3"),
        ]
        .into_iter()
        .map(|(id, hash)| (id.to_owned(), hash.to_owned()))
        .collect()
    }

    fn claim(statement: &str, citations: &[&str]) -> Claim {
        Claim {
            statement: statement.into(),
            kind: ClaimKind::Implemented,
            citations: citations.iter().map(|id| (*id).to_owned()).collect(),
        }
    }

    fn finish(claims: Vec<Claim>) -> SectionDraftFinish {
        SectionDraftFinish {
            address: address(),
            claims,
        }
    }

    fn refusal(claims: Vec<Claim>) -> DraftRefusal {
        admit(&address(), &set(), finish(claims)).unwrap_err()
    }

    // AC-0664: a valid finish is admitted unchanged, with the hash of each
    // distinct cited fact and of no other.
    #[test]
    fn a_valid_finish_is_admitted_with_the_hashes_of_what_it_cites() {
        let claims = vec![
            claim("A user can check out.", &["F-0000000000000001"]),
            Claim {
                kind: ClaimKind::Interpretation,
                ..claim(
                    "Checkout is limited per day.",
                    &["F-0000000000000002", "F-0000000000000001"],
                )
            },
        ];
        let draft = admit(&address(), &set(), finish(claims.clone())).unwrap();
        assert_eq!(draft.address, address());
        assert_eq!(draft.claims, claims);
        assert_eq!(
            draft.cited_hashes,
            BTreeMap::from([
                ("F-0000000000000001".to_owned(), "hash-1".to_owned()),
                ("F-0000000000000002".to_owned(), "hash-2".to_owned()),
            ])
        );
    }

    #[test]
    fn a_finish_for_another_address_is_refused() {
        for change in [
            |a: &mut SectionAddress| a.section = "1.2".into(),
            |a: &mut SectionAddress| a.document = "D2".into(),
            |a: &mut SectionAddress| a.scope = "F-0000000000000001".into(),
            |a: &mut SectionAddress| a.catalogue_version = "1.1".into(),
        ] {
            let mut other = finish(vec![claim(
                "A user can check out.",
                &["F-0000000000000001"],
            )]);
            change(&mut other.address);
            assert_eq!(
                admit(&address(), &set(), other),
                Err(DraftRefusal::AddressMismatch)
            );
        }
    }

    #[test]
    fn claim_count_is_bounded() {
        assert_eq!(refusal(Vec::new()), DraftRefusal::NoClaims);
        let one = claim("A user can check out.", &["F-0000000000000001"]);
        assert!(admit(&address(), &set(), finish(vec![one.clone(); MAX_CLAIMS])).is_ok());
        assert_eq!(
            refusal(vec![one; MAX_CLAIMS + 1]),
            DraftRefusal::TooManyClaims
        );
    }

    #[test]
    fn a_statement_is_bounded_plain_text_without_secrets() {
        let cited = ["F-0000000000000001"];
        let good = claim("A user can check out.", &cited);
        assert!(
            admit(
                &address(),
                &set(),
                finish(vec![claim(&"a".repeat(MAX_STATEMENT_BYTES), &cited)])
            )
            .is_ok()
        );
        assert_eq!(
            refusal(vec![
                good.clone(),
                claim(&"a".repeat(MAX_STATEMENT_BYTES + 1), &cited)
            ]),
            DraftRefusal::StatementTooLong { claim: 1 }
        );
        for blank in ["", "   ", "...", "—"] {
            assert_eq!(
                refusal(vec![claim(blank, &cited)]),
                DraftRefusal::BlankStatement { claim: 0 },
                "{blank:?}"
            );
        }
        for control in ["line one\nline two", "tab\there", "bell\u{7}"] {
            assert_eq!(
                refusal(vec![claim(control, &cited)]),
                DraftRefusal::StatementNotPlainText { claim: 0 }
            );
        }
        assert_eq!(
            refusal(vec![claim(
                "The key is AKIAIOSFODNN7EXAMPLE in production.",
                &cited
            )]),
            DraftRefusal::StatementHoldsSecret { claim: 0 }
        );
    }

    #[test]
    fn a_claim_cites_one_to_sixteen_distinct_admitted_ids() {
        assert_eq!(
            refusal(vec![claim("A user can check out.", &[])]),
            DraftRefusal::NoCitation { claim: 0 }
        );
        let many: Vec<String> = (0..=MAX_CITATIONS).map(|n| format!("F-{n:016x}")).collect();
        let wide: BTreeMap<String, String> =
            many.iter().map(|id| (id.clone(), "hash".into())).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(
            admit(
                &address(),
                &wide,
                finish(vec![claim("A user can check out.", &refs[..MAX_CITATIONS])])
            )
            .is_ok()
        );
        assert_eq!(
            admit(
                &address(),
                &wide,
                finish(vec![claim("A user can check out.", &refs)])
            ),
            Err(DraftRefusal::TooManyCitations { claim: 0 })
        );
        assert_eq!(
            refusal(vec![claim(
                "A user can check out.",
                &["F-0000000000000001", "F-0000000000000001"]
            )]),
            DraftRefusal::DuplicateCitation { claim: 0 }
        );
        // One id outside the set refuses the whole finish.
        assert_eq!(
            refusal(vec![
                claim("A user can check out.", &["F-0000000000000001"]),
                claim(
                    "A user can refund.",
                    &["F-0000000000000002", "F-00000000000000ff"]
                ),
            ]),
            DraftRefusal::CitationNotAdmitted { claim: 1 }
        );
    }

    #[test]
    fn a_refusal_is_a_closed_code_that_echoes_nothing_from_the_finish() {
        let refused = refusal(vec![claim("private text here", &["F-private-id"])]);
        let wire = serde_json::to_string(&refused).unwrap();
        assert_eq!(wire, r#"{"reason":"citation_not_admitted","claim":0}"#);
        assert_eq!(refused.code(), "citation_not_admitted");
        assert!(!wire.contains("private"));
    }

    #[test]
    fn a_finish_with_text_outside_its_claims_does_not_parse() {
        let extra = serde_json::json!({
            "address": address(),
            "claims": [],
            "summary": "prose outside any claim"
        });
        assert!(serde_json::from_value::<SectionDraftFinish>(extra).is_err());
        let uncited = serde_json::json!({
            "address": address(),
            "claims": [{ "statement": "A user can check out.", "kind": "implemented" }]
        });
        assert!(serde_json::from_value::<SectionDraftFinish>(uncited).is_err());
    }
}
