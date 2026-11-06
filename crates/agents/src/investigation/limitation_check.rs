//! Recognize direct denials of a captured source identity. Qualified denials
//! (registration, other callers, full coverage, etc.) have a different subject.
use super::*;
use crate::{TaskCaptureSpanRef, TaskFactKey};

type Capture<'a> = (&'a TaskCaptureSpanRef, Option<&'a str>, &'a String);

pub(super) fn correction<'a>(
    ledger: &InvestigationInputLedger,
    limitations: impl Iterator<Item = (&'a str, &'a [String])>,
) -> Option<ProseCorrection> {
    let mut conflicts = BTreeSet::new();
    let captures = ledger
        .citations
        .iter()
        .filter_map(|citation| {
            let InvestigationEvidenceOrigin::CapturedPrimarySource { captured, .. } =
                &citation.origin
            else {
                return None;
            };
            let symbol = match &citation.fact {
                TaskFactKey::Node { id } if id.starts_with("sym:") || id.starts_with("decl:") => {
                    id.rsplit_once('#').map(|(_, symbol)| symbol)
                }
                _ => None,
            };
            Some((captured, symbol, &citation.citation_id))
        })
        .collect::<Vec<_>>();
    for (limitation, cited) in limitations {
        for clause in limitation
            .split([';', '\n'])
            .flat_map(|part| part.split(". "))
        {
            let normalized = normalize_negation(clause.trim().trim_end_matches('.'));
            let clause = normalized.as_str();
            let exclusive = clause.strip_prefix("Only ").and_then(|s| {
                s.strip_suffix(" was read")
                    .or_else(|| s.strip_suffix(" was acquired"))
            });
            let denied = denied_name(clause);
            let comparative = denied.and_then(|s| s.split_once(" code paths other than "));
            if let Some(subject) = exclusive.or(comparative.map(|(_, subject)| subject)) {
                exclusive_conflicts(
                    &captures,
                    subject,
                    comparative.map(|(area, _)| area),
                    cited,
                    &mut conflicts,
                );
                continue;
            }
            let Some(name) = denied else { continue };
            let matches = captures
                .iter()
                .filter(|(span, symbol, _)| names_source(name, &span.file.path, *symbol))
                .map(|(span, symbol, id)| {
                    (&span.file.source_id, span.file.path.as_str(), *symbol, *id)
                })
                .collect::<Vec<_>>();
            // An unqualified symbol that resolves to multiple files or definitions
            // cannot identify which one the author meant.
            if let Some(&(source, path, symbol, _)) = matches.first()
                && matches.iter().all(|&(src, p, s, _)| {
                    src == source && p == path && (name.trim_matches('`') == path || s == symbol)
                })
            {
                conflicts.extend(matches.into_iter().map(|(_, _, _, id)| id.clone()));
            }
        }
    }
    (!conflicts.is_empty()).then(|| {
        ProseCorrection::LimitationDeniesAcquiredEvidence(Box::new(ConflictingEvidence {
            evidence_ids: conflicts.into_iter().collect(),
        }))
    })
}

// A relative range needs a unique finding-local anchor. A named method can
// also resolve from the ledger, but another method must prove a distinct path.
fn exclusive_conflicts(
    captures: &[Capture<'_>],
    subject: &str,
    area: Option<&str>,
    cited: &[String],
    conflicts: &mut BTreeSet<String>,
) {
    let name = subject.strip_prefix("the captured ").unwrap_or(subject);
    let name = name
        .strip_suffix(" method")
        .or_else(|| name.strip_suffix(" function"))
        .unwrap_or(name);
    let mut anchors = captures.iter().filter(|(span, symbol, id)| {
        if subject == "the captured byte range" {
            return cited.contains(id);
        }
        symbol.is_some()
            && names_source(name, "", *symbol)
            && area.is_none_or(|area| {
                span.file
                    .path
                    .split('/')
                    .any(|part| part.eq_ignore_ascii_case(area))
            })
    });
    if let Some((anchor, anchor_symbol, _)) = anchors.next()
        && anchors.all(|(span, symbol, _)| span == anchor && symbol == anchor_symbol)
    {
        for (other, symbol, id) in captures {
            if other.file == anchor.file
                && (other.byte_start < anchor.byte_start || other.byte_end > anchor.byte_end)
                && (subject == "the captured byte range"
                    || (symbol.is_some()
                        && symbol != anchor_symbol
                        && (other.byte_end <= anchor.byte_start
                            || other.byte_start >= anchor.byte_end)))
            {
                conflicts.insert((*id).clone());
            }
        }
    }
}

// Rewrite common direct-negative forms ("wasn't read", "was never read",
// "We never read X") to the "not" forms the denial parser understands.
fn normalize_negation(clause: &str) -> String {
    let mut text = clause.replace('\u{2019}', "'");
    for (from, to) in [
        ("wasn't ", "was not "),
        ("weren't ", "were not "),
        ("isn't ", "is not "),
        ("aren't ", "are not "),
        ("didn't ", "did not "),
        (" was never ", " was not "),
        (" were never ", " were not "),
        (" is never ", " is not "),
        (" are never ", " are not "),
    ] {
        text = text.replace(from, to);
    }
    for subject in ["We", "I"] {
        if let Some(rest) = text.strip_prefix(&format!("{subject} never ")) {
            text = format!("{subject} did not {rest}");
        }
    }
    text
}

fn denied_name(clause: &str) -> Option<&str> {
    // A consequence introduced by ", so" does not qualify the denial.
    let words = clause
        .split_once(", so ")
        .map_or(clause, |(claim, _)| claim)
        .trim()
        .trim_end_matches('.');
    let (prefix, rest) = words.split_once(" not ")?;
    let is_read = |verb: &str| {
        matches!(
            verb,
            "read"
                | "acquired"
                | "captured"
                | "covered"
                | "inspected"
                | "examined"
                | "traced"
                | "reviewed"
        )
    };
    if matches!(prefix, "We did" | "I did") {
        let (verb, subject) = rest.split_once(' ')?;
        // "We never inspected X" normalizes to "We did not inspected X".
        return (matches!(
            verb,
            "read" | "acquire" | "capture" | "cover" | "inspect" | "examine" | "trace" | "review"
        ) || is_read(verb))
        .then_some(subject);
    }
    let (subject, auxiliary) = prefix.rsplit_once(' ')?;
    if is_read(rest) && matches!(auxiliary, "was" | "were" | "is" | "are") {
        // A shared qualifier in "other callers of X and Y" also applies to Y.
        return Some(
            subject
                .split_once(" and ")
                .map_or(subject, |(first, _)| first),
        );
    }
    None
}

fn names_source(name: &str, path: &str, symbol: Option<&str>) -> bool {
    let name = name.trim();
    let name = name
        .strip_prefix("The ")
        .or_else(|| name.strip_prefix("the "))
        .unwrap_or(name);
    let name = name.trim_matches('`');
    if name == path {
        return true;
    }
    let Some(symbol) = symbol else { return false };
    let leaf = symbol.rsplit('.').next().unwrap_or(symbol);
    let leaf = leaf.split('@').next().unwrap_or(leaf);
    if name == symbol || name == leaf {
        return true;
    }
    // A scoped handler description names a path component and a handler
    // symbol: e.g. "background script's random-alias handler" resolves to
    // background/create-alias.js#handleNewRandomAlias. Never match a mere
    // mention of "handler", or a claim about its registration or callers.
    let Some((directory, description)) = name.split_once(" script's ") else {
        return false;
    };
    let Some(noun) = description.strip_suffix(" handler") else {
        return false;
    };
    let Some(handled) = leaf.strip_prefix("handle") else {
        return false;
    };
    let handled = handled.strip_prefix("New").unwrap_or(handled);
    path.split('/').any(|component| component == directory)
        && noun
            .split('-')
            .all(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_lowercase()))
        && noun.replace('-', "").eq_ignore_ascii_case(handled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run6_denied_handler_and_true_coverage_limits() {
        let (_, mut ledger, _) = crate::investigation::tests::inspection_tests::fixture();
        let citation = &mut ledger.citations[1];
        citation.citation_id = "evidence-6".into();
        citation.fact = TaskFactKey::Node {
            id: "sym:repo@src/background/create-alias.js#handleNewRandomAlias".into(),
        };
        let InvestigationEvidenceOrigin::CapturedPrimarySource { captured, .. } =
            &mut citation.origin
        else {
            unreachable!()
        };
        captured.file.path = "src/background/create-alias.js".into();
        let matches = |text: &str| {
            correction(&ledger, [(text, &[][..])].into_iter())
                == Some(ProseCorrection::LimitationDeniesAcquiredEvidence(Box::new(
                    ConflictingEvidence {
                        evidence_ids: vec!["evidence-6".into()],
                    },
                )))
        };
        assert!(matches(
            "Only the popup entry point for random alias creation was acquired; the background script's random-alias handler and context-menu flow were not read, so the full set of callers is unknown."
        ));
        assert!(!matches(
            "Only the background create-alias module's random-alias handler was acquired; how this handler is registered (e.g., as a message or command listener) is not covered by the acquired ranges."
        ));
        assert!(!matches(
            "Coverage is limited to the acquired source ranges for the popup Main.vue alias methods, APIService route declarations, background create-alias module, and context-menu injected-script helper; other alias-related code paths were not acquired."
        ));
        assert!(matches("src/background/create-alias.js was not read."));
        assert!(matches("`handleNewRandomAlias` was not captured."));
        assert!(!matches(
            "Other callers of handleNewRandomAlias were not read."
        ));
        assert!(!matches("handleNewRandomAlias was not captured in full."));
        // Contractions and "never" forms deny the same acquired evidence.
        for text in [
            "src/background/create-alias.js wasn't read.",
            "src/background/create-alias.js wasn\u{2019}t read.",
            "src/background/create-alias.js was never read.",
            "We never read src/background/create-alias.js.",
            "We didn't read src/background/create-alias.js.",
            "I never inspected handleNewRandomAlias.",
            "`handleNewRandomAlias` wasn't captured.",
        ] {
            assert!(matches(text), "{text}");
        }
        assert!(!matches(
            "Other callers of handleNewRandomAlias weren't read."
        ));
        assert!(!matches("handleNewRandomAlias was never captured in full."));
        assert!(!matches("We never read the context-menu wiring."));
    }

    #[test]
    fn run7_finding_limits_resolve_only_unambiguous_captured_witnesses() {
        let (_, mut ledger, _) = crate::investigation::tests::inspection_tests::fixture();
        let template = ledger.citations[1].clone();
        let popup = "src/popup/components/Main.vue";
        let background = "src/background/create-alias.js";
        ledger.citations = [
            (
                "evidence-1",
                background,
                "sym",
                "handleNewRandomAlias",
                163,
                901,
            ),
            (
                "evidence-2",
                popup,
                "sym",
                "methods@8608.createCustomAlias@11494",
                11494,
                13252,
            ),
            (
                "evidence-4",
                popup,
                "sym",
                "methods@8608.createRandomAlias@13259",
                13259,
                14191,
            ),
            ("evidence-6", background, "rule", "exit@605", 605, 714),
        ]
        .into_iter()
        .map(|(id, path, prefix, symbol, start, end)| {
            let mut citation = template.clone();
            citation.citation_id = id.into();
            citation.fact = TaskFactKey::Node {
                id: format!("{prefix}:repo@{path}#{symbol}"),
            };
            let InvestigationEvidenceOrigin::CapturedPrimarySource { captured, .. } =
                &mut citation.origin
            else {
                unreachable!()
            };
            captured.file.path = path.into();
            captured.byte_start = start;
            captured.byte_end = end;
            citation
        })
        .collect();
        let check = |text: &str, cited: &[&str], expected: Option<&str>| {
            let ids: Vec<_> = cited.iter().map(|s| s.to_string()).collect();
            assert_eq!(
                correction(&ledger, [(text, ids.as_slice())].into_iter()),
                expected.map(
                    |id| ProseCorrection::LimitationDeniesAcquiredEvidence(Box::new(
                        ConflictingEvidence {
                            evidence_ids: vec![id.into()]
                        }
                    ))
                ),
                "{text}"
            );
        };
        check(
            "Popup code paths other than the captured createCustomAlias method were not inspected.",
            &["evidence-2"],
            Some("evidence-4"),
        );
        let only =
            "Only the captured byte range was read; surrounding handler wiring was not inspected.";
        check(only, &["evidence-6"], Some("evidence-1"));
        check(only, &["evidence-1"], None);
        check(only, &[], None);
        check(only, &["evidence-1", "evidence-6"], None);
        check(
            "Only createCustomAlias was acquired.",
            &[],
            Some("evidence-4"),
        );
        check(
            "How this background handler is invoked (e.g., from a context menu) was not established by the inspected evidence.",
            &["evidence-1"],
            None,
        );
        for verb in ["inspect", "examine", "trace", "review"] {
            check(
                &format!("We did not {verb} createRandomAlias."),
                &[],
                Some("evidence-4"),
            );
        }
    }
}
