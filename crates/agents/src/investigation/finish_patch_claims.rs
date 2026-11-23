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

/// The words of a sentence, borrowed from it.
fn words(sentence: &str) -> impl Iterator<Item = &str> {
    sentence
        .split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '#' || c == '\''))
        .filter(|word| !word.is_empty())
}

fn starts_with(word: &str, prefix: &str) -> bool {
    word.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

fn number(word: &str) -> Option<usize> {
    word.trim_start_matches('#').parse().ok()
}

/// Whether `sentence` claims an edit `made` lacks. Nothing is allocated: the
/// sentence is read in place, ASCII case-insensitively.
fn claims(sentence: &str, verbs: &[&str], made: &BTreeSet<usize>) -> bool {
    let has = |list: &[&str]| {
        words(sentence).any(|word| list.iter().any(|item| word.eq_ignore_ascii_case(item)))
    };
    if !words(sentence).any(|word| starts_with(word, "finding")) || !has(verbs) || has(NEGATION) {
        return false;
    }
    // Every number a finding reference names, 1-based: "finding 7",
    // "finding-7", "finding #7", and lists such as "findings 2, 3 and 5".
    let mut numbered = false;
    let mut in_list = false;
    for word in words(sentence) {
        let named = if starts_with(word, "finding-") {
            in_list = true;
            number(&word["finding-".len()..])
        } else if starts_with(word, "finding") {
            in_list = true;
            continue;
        } else if in_list {
            match number(word) {
                Some(n) => Some(n),
                None if word.eq_ignore_ascii_case("and") || word.eq_ignore_ascii_case("or") => {
                    continue;
                }
                None => {
                    in_list = false;
                    None
                }
            }
        } else {
            None
        };
        if let Some(n) = named {
            numbered = true;
            if n == 0 || !made.contains(&(n - 1)) {
                return true;
            }
        }
    }
    // An unnumbered claim ("its finding was withdrawn") needs at least one
    // such edit.
    !numbered && made.is_empty()
}

/// Refuse new limitation `texts` that claim a finding was withdrawn or
/// replaced when it was not. `withdrawn` and `patched` hold the original
/// finding indices (0-based) the merge actually withdrew and replaced.
pub(super) fn check<'a>(
    texts: impl IntoIterator<Item = &'a str>,
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
                check([text], &set(&[]), &set(&[])),
                Err(PatchRejection::UnsupportedEditClaim),
                "{text}"
            );
        }
    }

    #[test]
    fn true_or_negated_claims_and_unrelated_prose_pass() {
        // Finding 7 really was withdrawn (index 6).
        assert_eq!(
            check(["Finding 7 is withdrawn."], &set(&[6]), &set(&[])),
            Ok(())
        );
        // A numbered claim must name a finding that was withdrawn.
        assert_eq!(
            check(["Finding 3 is withdrawn."], &set(&[6]), &set(&[])),
            Err(PatchRejection::UnsupportedEditClaim)
        );
        assert_eq!(
            check(["No finding was withdrawn."], &set(&[]), &set(&[])),
            Ok(())
        );
        assert_eq!(
            check(
                ["The finding is not withdrawn; it stays T3."],
                &set(&[]),
                &set(&[])
            ),
            Ok(())
        );
        // Code behavior that mentions removal is not an edit claim.
        assert_eq!(
            check(
                ["voidNamesRemovedFromOcl voids names removed from OCL."],
                &set(&[]),
                &set(&[])
            ),
            Ok(())
        );
        // A limitation may say a limitation was withdrawn to make room.
        assert_eq!(
            check(
                ["Limitation 2 was withdrawn to make room."],
                &set(&[]),
                &set(&[])
            ),
            Ok(())
        );
    }

    #[test]
    fn every_number_in_a_finding_list_is_checked() {
        // Only finding 2 (index 1) was withdrawn.
        for text in [
            "Findings 2 and 3 were withdrawn.",
            "findings 2, 3 or 5 were withdrawn",
            "finding-2 and finding-3 are withdrawn.",
        ] {
            assert_eq!(
                check([text], &set(&[1]), &set(&[])),
                Err(PatchRejection::UnsupportedEditClaim),
                "{text}"
            );
        }
        assert_eq!(
            check(
                ["Findings 2 and 3 were withdrawn."],
                &set(&[1, 2]),
                &set(&[])
            ),
            Ok(())
        );
    }

    #[test]
    fn replacement_claims_must_match_a_patched_finding() {
        assert_eq!(
            check(
                ["finding-2 was corrected to cite L50-56."],
                &set(&[]),
                &set(&[1])
            ),
            Ok(())
        );
        assert_eq!(
            check(
                ["finding-4 was corrected to cite L50-56."],
                &set(&[]),
                &set(&[1])
            ),
            Err(PatchRejection::UnsupportedEditClaim)
        );
        assert_eq!(
            check(["The flagged finding was revised."], &set(&[]), &set(&[])),
            Err(PatchRejection::UnsupportedEditClaim)
        );
    }
}
