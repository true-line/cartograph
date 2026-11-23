//! @29 (#1728 run-29, AC-1434): a patch's new limitation prose must not
//! claim a finding edit the merge did not make.
//!
//! On run-29, three accepted disclosures said "finding 7 is withdrawn" or
//! "its finding was withdrawn" while the patch withdrew nothing, so the saved
//! result told readers a finding was gone that was still there. The host
//! counts the edits itself; this reads each sentence of the new limitation
//! text (disclosures and replaced limitations) and refuses one that asserts
//! a withdrawal or replacement of a finding that did not happen. A sentence
//! that negates the claim ("no finding was withdrawn") is not a claim.
use super::PatchRejection;
use std::collections::BTreeSet;

const WITHDRAW: &[&str] = &[
    "withdraw",
    "withdrawn",
    "withdrew",
    "withdrawing",
    "retracted",
];
const REPLACE: &[&str] = &["replaced", "revised", "rewritten", "amended", "corrected"];
const NEGATION: &[&str] = &[
    "no", "not", "none", "nothing", "never", "without", "isn't", "wasn't", "aren't", "weren't",
];

/// The 1-based finding numbers a sentence names, as "finding 7", "finding-7"
/// or "finding #7".
fn numbers(words: &[&str]) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, word) in words.iter().enumerate() {
        let rest = if let Some(rest) = word.strip_prefix("finding-") {
            Some(rest)
        } else if *word == "finding" {
            words.get(i + 1).copied()
        } else {
            None
        };
        if let Some(n) = rest
            .map(|r| r.trim_start_matches('#'))
            .and_then(|r| r.parse::<usize>().ok())
        {
            out.push(n);
        }
    }
    out
}

fn claims(sentence: &str, verbs: &[&str], made: &BTreeSet<usize>) -> bool {
    let lower = sentence.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '#' || c == '\''))
        .filter(|w| !w.is_empty())
        .collect();
    let names_finding = words.iter().any(|w| w.starts_with("finding"));
    if !names_finding
        || !words.iter().any(|w| verbs.contains(w))
        || words.iter().any(|w| NEGATION.contains(w))
    {
        return false;
    }
    let named = numbers(&words);
    if named.is_empty() {
        // An unnumbered claim ("its finding was withdrawn") needs at least
        // one such edit.
        return made.is_empty();
    }
    named.iter().any(|n| *n == 0 || !made.contains(&(n - 1)))
}

/// Refuse new limitation `texts` that claim a finding was withdrawn or
/// replaced when it was not. `withdrawn` and `patched` hold the original
/// finding indices (0-based) the merge actually withdrew and replaced.
pub(super) fn check(
    texts: &[&str],
    withdrawn: &BTreeSet<usize>,
    patched: &BTreeSet<usize>,
) -> Result<(), PatchRejection> {
    for text in texts {
        for sentence in text.split(['.', ';', '!', '?', '\n']) {
            if claims(sentence, WITHDRAW, withdrawn) || claims(sentence, REPLACE, patched) {
                return Err(PatchRejection::UnsupportedEditClaim);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[usize]) -> BTreeSet<usize> {
        items.iter().copied().collect()
    }

    #[test]
    fn run_29_withdrawal_claims_are_refused_when_nothing_was_withdrawn() {
        for text in [
            "de4dfb1b7680: The Item constructor was not read; finding 7 is withdrawn.",
            "dfa1d6b8ab16: getDisplayString was excluded by capacity. Its finding was withdrawn.",
            "eecfce17a97c: Deploy was excluded; the Deploy behavior finding is withdrawn.",
        ] {
            assert_eq!(
                check(&[text], &set(&[]), &set(&[])),
                Err(PatchRejection::UnsupportedEditClaim),
                "{text}"
            );
        }
    }

    #[test]
    fn true_or_negated_claims_and_unrelated_prose_pass() {
        // Finding 7 really was withdrawn (index 6).
        assert_eq!(
            check(&["Finding 7 is withdrawn."], &set(&[6]), &set(&[])),
            Ok(())
        );
        // A numbered claim must name a finding that was withdrawn.
        assert_eq!(
            check(&["Finding 3 is withdrawn."], &set(&[6]), &set(&[])),
            Err(PatchRejection::UnsupportedEditClaim)
        );
        assert_eq!(
            check(&["No finding was withdrawn."], &set(&[]), &set(&[])),
            Ok(())
        );
        assert_eq!(
            check(
                &["The finding is not withdrawn; it stays T3."],
                &set(&[]),
                &set(&[])
            ),
            Ok(())
        );
        // Code behavior that mentions removal is not an edit claim.
        assert_eq!(
            check(
                &["voidNamesRemovedFromOcl voids names removed from OCL."],
                &set(&[]),
                &set(&[])
            ),
            Ok(())
        );
        // A limitation may say a limitation was withdrawn to make room.
        assert_eq!(
            check(
                &["Limitation 2 was withdrawn to make room."],
                &set(&[]),
                &set(&[])
            ),
            Ok(())
        );
    }

    #[test]
    fn replacement_claims_must_match_a_patched_finding() {
        assert_eq!(
            check(
                &["finding-2 was corrected to cite L50-56."],
                &set(&[]),
                &set(&[1])
            ),
            Ok(())
        );
        assert_eq!(
            check(
                &["finding-4 was corrected to cite L50-56."],
                &set(&[]),
                &set(&[1])
            ),
            Err(PatchRejection::UnsupportedEditClaim)
        );
        assert_eq!(
            check(&["The flagged finding was revised."], &set(&[]), &set(&[])),
            Err(PatchRejection::UnsupportedEditClaim)
        );
    }
}
