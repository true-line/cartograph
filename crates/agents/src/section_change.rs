//! What changed between a section draft and the draft it replaces (#1749,
//! SPEC-17, ADR-0045 decision 7, AC-0675).
//!
//! The comparison reads the two stored drafts and nothing else: no graph,
//! no model and no review. A claim is kept when its statement, kind and
//! citation list are equal. A cited fact is changed when both drafts cite
//! it and recorded another content hash for it.

use crate::section_draft::{Claim, SectionDraft};
use serde::Serialize;

/// The change a successor makes to the draft it replaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SectionChange {
    /// Claims of the successor the replaced draft does not have, in the
    /// successor's order.
    pub claims_added: Vec<Claim>,
    /// Claims of the replaced draft the successor does not have, in the
    /// replaced draft's order.
    pub claims_removed: Vec<Claim>,
    /// Claims both have, in the successor's order.
    pub claims_kept: Vec<Claim>,
    /// Citation ids only the successor cites, sorted.
    pub facts_added: Vec<String>,
    /// Citation ids only the replaced draft cites, sorted.
    pub facts_removed: Vec<String>,
    /// Citation ids both cite with another recorded hash, sorted.
    pub facts_changed: Vec<String>,
}

/// Compare `successor` with the draft it replaces. A claim that appears
/// twice is matched once per appearance.
pub fn section_change(replaced: &SectionDraft, successor: &SectionDraft) -> SectionChange {
    let mut unmatched: Vec<Option<&Claim>> = replaced.claims.iter().map(Some).collect();
    let mut claims_added = Vec::new();
    let mut claims_kept = Vec::new();
    for claim in &successor.claims {
        match unmatched.iter_mut().find(|slot| **slot == Some(claim)) {
            Some(slot) => {
                *slot = None;
                claims_kept.push(claim.clone());
            }
            None => claims_added.push(claim.clone()),
        }
    }
    let claims_removed = unmatched.into_iter().flatten().cloned().collect();

    let mut facts_added = Vec::new();
    let mut facts_changed = Vec::new();
    for (id, hash) in &successor.cited_hashes {
        match replaced.cited_hashes.get(id) {
            None => facts_added.push(id.clone()),
            Some(recorded) if recorded != hash => facts_changed.push(id.clone()),
            Some(_) => {}
        }
    }
    let facts_removed = replaced
        .cited_hashes
        .keys()
        .filter(|id| !successor.cited_hashes.contains_key(*id))
        .cloned()
        .collect();
    SectionChange {
        claims_added,
        claims_removed,
        claims_kept,
        facts_added,
        facts_removed,
        facts_changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section_draft::{ClaimKind, SYSTEM_SCOPE, SectionAddress};
    use std::collections::BTreeMap;

    fn claim(statement: &str, kind: ClaimKind, citations: &[&str]) -> Claim {
        Claim {
            statement: statement.into(),
            kind,
            citations: citations.iter().map(|id| (*id).to_owned()).collect(),
        }
    }

    fn draft(claims: Vec<Claim>, hashes: &[(&str, &str)]) -> SectionDraft {
        SectionDraft {
            address: SectionAddress {
                catalogue_version: "1.0".into(),
                document: "D1".into(),
                section: "1.3".into(),
                scope: SYSTEM_SCOPE.into(),
            },
            claims,
            cited_hashes: hashes
                .iter()
                .map(|(id, hash)| ((*id).to_owned(), (*hash).to_owned()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    const A: &str = "F-000000000000000a";
    const B: &str = "F-000000000000000b";
    const C: &str = "F-000000000000000c";
    const D: &str = "F-000000000000000d";

    // AC-0675 (T-0675): claims added, removed and kept, and cited facts
    // added, removed and changed, from the two drafts alone.
    #[test]
    fn the_change_lists_claims_and_cited_facts_from_the_two_drafts() {
        let kept = claim("A user can check out.", ClaimKind::Implemented, &[A]);
        let removed = claim("Refunds are planned.", ClaimKind::DocumentedIntent, &[B]);
        let reworded_before = claim("Carts expire.", ClaimKind::Implemented, &[C]);
        let reworded_after = claim("Carts expire after a day.", ClaimKind::Implemented, &[C]);
        let added = claim("Stock is checked.", ClaimKind::Implemented, &[D]);
        let replaced = draft(
            vec![kept.clone(), removed.clone(), reworded_before.clone()],
            &[(A, "a1"), (B, "b1"), (C, "c1")],
        );
        let successor = draft(
            vec![added.clone(), kept.clone(), reworded_after.clone()],
            &[(A, "a1"), (C, "c2"), (D, "d1")],
        );
        assert_eq!(
            section_change(&replaced, &successor),
            SectionChange {
                claims_added: vec![added, reworded_after],
                claims_removed: vec![removed, reworded_before],
                claims_kept: vec![kept],
                facts_added: vec![D.to_owned()],
                facts_removed: vec![B.to_owned()],
                facts_changed: vec![C.to_owned()],
            }
        );
    }

    #[test]
    fn a_claim_is_kept_only_when_statement_kind_and_citations_are_equal() {
        let base = claim("A user can check out.", ClaimKind::Implemented, &[A, B]);
        let replaced = draft(vec![base.clone()], &[(A, "a1"), (B, "b1")]);
        for other in [
            claim("A user can check out!", ClaimKind::Implemented, &[A, B]),
            claim("A user can check out.", ClaimKind::Interpretation, &[A, B]),
            claim("A user can check out.", ClaimKind::Implemented, &[B, A]),
            claim("A user can check out.", ClaimKind::Implemented, &[A]),
        ] {
            let change = section_change(
                &replaced,
                &draft(vec![other.clone()], &[(A, "a1"), (B, "b1")]),
            );
            assert_eq!(change.claims_added, vec![other]);
            assert_eq!(change.claims_removed, vec![base.clone()]);
            assert!(change.claims_kept.is_empty());
        }
        // The same draft changes nothing, and a repeated claim is matched
        // once per appearance.
        let same = section_change(&replaced, &replaced);
        assert_eq!(same.claims_kept, vec![base.clone()]);
        assert!(same.claims_added.is_empty() && same.claims_removed.is_empty());
        assert!(
            same.facts_added.is_empty()
                && same.facts_removed.is_empty()
                && same.facts_changed.is_empty()
        );
        let twice = draft(vec![base.clone(), base.clone()], &[(A, "a1"), (B, "b1")]);
        let change = section_change(&replaced, &twice);
        assert_eq!(change.claims_kept, vec![base.clone()]);
        assert_eq!(change.claims_added, vec![base]);
    }
}
