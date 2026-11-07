//! Conservative verdict adapter over the shared diagnostic grammar.
#[path = "literal.rs"]
pub(crate) mod literal;

/// Historical applicability and decisions, derived from the same classification
/// that the host can retain. No separate grammar can drift from the verdict.
pub(crate) fn check(source: &str, sentence: &str) -> Option<bool> {
    super::super::claim_diagnostics::enumeration::explain(source, sentence)
        .map(|result| result.is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    const SOURCE: &str = include_str!("r3-transitions.txt");

    #[test]
    fn duplicate_table_keys_are_not_applicable() {
        assert_eq!(check("{A: ['T'], A: ['T']}", "Only A lists T"), None);
    }

    // AC-0287: exact r3 text distinguishes false universals from ordinary prose.
    #[test]
    fn r3_claims_and_true_empty_list() {
        assert_eq!(
            check(
                SOURCE,
                "The transition table shows only the ArrangingPayment state can begin payment authorization or settlement, and that delivery and shipping states branch from PaymentSettled."
            ),
            Some(false)
        );
        assert_eq!(
            check(
                SOURCE,
                "Every pre-cancellation state lists Cancelled as a permitted target, indicating cancellation is a universal exit available throughout the pre-cancelled lifecycle, while Cancelled is terminal in this table."
            ),
            Some(false)
        );
        assert_eq!(
            check(
                SOURCE,
                "Each state entry lists only the target states it allows, and Cancelled permits no further transitions."
            ),
            Some(true)
        );
        assert_eq!(
            check(SOURCE, "Cancelled lists no further transitions"),
            Some(true)
        );
        assert_eq!(
            check(
                SOURCE,
                "Cancelled is terminal and has no outgoing transitions"
            ),
            Some(false)
        );
        assert_eq!(
            check(
                SOURCE,
                "Every state except Created and Modifying lists Cancelled"
            ),
            Some(false)
        );
        assert_eq!(
            check(
                SOURCE,
                "Each state entry lists only the target states it allows"
            ),
            None
        );
    }

    #[test]
    fn bare_arrays_require_explicit_membership_predicates() {
        assert_eq!(check("['Target']", "All entries are Target"), Some(true));
        assert_eq!(
            check("['Target', 'Other']", "All entries are Target"),
            Some(false)
        );
        assert_eq!(check("['Target']", "Only Target is listed"), Some(true));
        assert_eq!(
            check("['Target']", "Only Target authorizes payment"),
            Some(false)
        );
    }

    #[test]
    fn explicit_true_and_false_relations() {
        let source = "{A: ['Target'], B: ['Target']}";
        assert_eq!(check(source, "Every state lists Target"), Some(true));
        assert_eq!(check(source, "Only A lists Target"), Some(false));
        assert_eq!(
            check("{A: ['Target'], B: []}", "Only A lists Target"),
            Some(true)
        );
        assert_eq!(check(source, "A has no outgoing transitions"), Some(false));
        assert_eq!(
            check(source, "Every selected state lists Target"),
            Some(false)
        );
        assert_eq!(
            check(source, "Every state does not list Target"),
            Some(false)
        );
        assert_eq!(check(source, "Only unrelated prose is available"), None);
        for claim in [
            "Only A forbids Target but lists Target",
            "Only A lists Target conditionally",
            "Every state lists Target while preventing it",
            "B says A has no outgoing transitions",
        ] {
            assert_eq!(check(source, claim), Some(false), "{claim}");
        }
    }
}
