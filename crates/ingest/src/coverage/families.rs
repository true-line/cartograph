//! Extractor families with no producer for a language present in the run
//! (#2164, AC-1306).
//!
//! A built-in adapter covers a language's syntax, but not every adapter has
//! every extractor: a repository can hold sources of a supported language
//! whose target tests, persisted data entities, business-rule evidence or
//! endpoint authentication no installed producer recovers. Each such pair is
//! a not-covered capability on the inventory (R-INT-4), so an absent test,
//! rule or guard of that language is never read as checked.
//!
//! [`PRODUCERS`] states which families each built-in adapter language has a
//! producer for. Its tests keep it in step with the adapter crates and with
//! the endpoint permission recoveries the spec compiler reads.

use super::UnsupportedCapability;
use crate::preflight::LanguageDetection;

/// Capability prefix of a family with no producer for a language: the
/// capability is `extractor-not-run:<family>:<language>`.
pub const MISSING_FAMILY_CAPABILITY: &str = "extractor-not-run";

/// One extractor family the coverage inventory checks per language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Target test cases and the requests they make.
    TargetTests,
    /// Persisted data entities (`DataEntity`).
    Persistence,
    /// Business-rule evidence.
    RuleEvidence,
    /// Endpoint authentication guards and permission sites.
    EndpointAuth,
}

impl Family {
    /// Every family, in the order the inventory lists them.
    pub const ALL: [Family; 4] = [
        Family::TargetTests,
        Family::Persistence,
        Family::RuleEvidence,
        Family::EndpointAuth,
    ];

    /// Wire spelling inside the capability.
    pub fn as_str(self) -> &'static str {
        match self {
            Family::TargetTests => "target-tests",
            Family::Persistence => "persistence",
            Family::RuleEvidence => "rule-evidence",
            Family::EndpointAuth => "endpoint-auth",
        }
    }

    /// What the family recovers, in words.
    fn recovers(self) -> &'static str {
        match self {
            Family::TargetTests => "target test cases",
            Family::Persistence => "persisted data entities",
            Family::RuleEvidence => "business-rule evidence",
            Family::EndpointAuth => "endpoint authentication guards",
        }
    }
}

use Family::{EndpointAuth, Persistence, RuleEvidence, TargetTests};

/// The built-in adapter languages the families are checked for, each with
/// the families an installed producer recovers for it. A language not
/// listed here (Vue single-file components, WebExtension manifests,
/// Terraform, a plugin's language) is not checked.
pub const PRODUCERS: &[(&str, &[Family])] = &[
    (
        "TypeScript",
        &[TargetTests, Persistence, RuleEvidence, EndpointAuth],
    ),
    ("JavaScript", &[TargetTests, Persistence, RuleEvidence]),
    (
        "Python",
        &[TargetTests, Persistence, RuleEvidence, EndpointAuth],
    ),
    (
        "Go",
        &[TargetTests, Persistence, RuleEvidence, EndpointAuth],
    ),
    (
        "Rust",
        &[TargetTests, Persistence, RuleEvidence, EndpointAuth],
    ),
    (
        "Java",
        &[TargetTests, Persistence, RuleEvidence, EndpointAuth],
    ),
    ("Kotlin", &[TargetTests, Persistence, RuleEvidence]),
];

/// One not-covered entry for each family with no producer for a language
/// the run detected, in detection order and then family order. The site is
/// the adapter that read the language, and the count its source files.
pub(super) fn missing(languages: &[LanguageDetection]) -> Vec<UnsupportedCapability> {
    let mut out = Vec::new();
    for detection in languages {
        let (Some(adapter), Some((_, families))) = (
            detection.adapter.as_deref(),
            PRODUCERS
                .iter()
                .find(|(language, _)| *language == detection.language),
        ) else {
            continue;
        };
        if detection.files == 0 {
            continue;
        }
        for family in Family::ALL {
            if families.contains(&family) {
                continue;
            }
            let language = &detection.language;
            out.push(UnsupportedCapability {
                capability: format!("{MISSING_FAMILY_CAPABILITY}:{}:{language}", family.as_str()),
                occurrences: detection.files,
                first_site: adapter.to_string(),
                reason: format!(
                    "{} {language} file(s) were read, but no installed producer recovers {} \
                     for {language}, so none is stated for them: a tool limitation, not a \
                     system gap",
                    detection.files,
                    family.recovers()
                ),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn crates_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates directory")
            .to_path_buf()
    }

    /// The adapter crate that holds a language's extractors.
    fn adapter_crate(language: &str) -> &'static str {
        match language {
            "TypeScript" | "JavaScript" => "adapters-lang-ts",
            "Python" => "adapters-lang-python",
            "Go" => "adapters-lang-go",
            "Rust" => "adapters-lang-rust",
            "Java" => "adapters-lang-java",
            "Kotlin" => "adapters-lang-kotlin",
            other => panic!("no adapter crate for {other}"),
        }
    }

    /// The sources of a crate's `src` directory, concatenated.
    fn sources(krate: &str) -> String {
        let mut text = String::new();
        let mut stack = vec![crates_dir().join(krate).join("src")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("adapter sources") {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    text.push_str(&std::fs::read_to_string(&path).unwrap());
                }
            }
        }
        text
    }

    /// The languages the spec compiler's endpoint permission recoveries
    /// name: the only readers of an endpoint's auth or permission site.
    fn auth_languages() -> BTreeSet<String> {
        let path = crates_dir().join("spec/src/workbench/endpoint_permissions.rs");
        let text = std::fs::read_to_string(path).expect("endpoint permissions source");
        let start = text.find("const RECOVERY: &[Recovery]").expect("RECOVERY");
        let end = start + text[start..].find("\n];").expect("end of RECOVERY");
        text[start..end]
            .split("language: \"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .map(str::to_string)
            .collect()
    }

    // AC-1306: the table claims a family only where its adapter has the
    // extractor, and endpoint auth exactly for the languages the spec
    // compiler recovers permission sites for.
    #[test]
    fn producer_table_matches_the_adapters_and_the_permission_recoveries() {
        let auth = auth_languages();
        for (language, families) in PRODUCERS {
            let src = crates_dir().join(adapter_crate(language)).join("src");
            for family in Family::ALL {
                let claimed = families.contains(&family);
                match family {
                    Family::TargetTests => assert_eq!(
                        claimed,
                        src.join("target_tests.rs").is_file(),
                        "{language} target tests: PRODUCERS disagrees with {}/target_tests.rs",
                        adapter_crate(language)
                    ),
                    Family::RuleEvidence => assert_eq!(
                        claimed,
                        src.join("rule_evidence.rs").is_file(),
                        "{language} rule evidence: PRODUCERS disagrees with {}/rule_evidence.rs",
                        adapter_crate(language)
                    ),
                    Family::Persistence => assert_eq!(
                        claimed,
                        sources(adapter_crate(language)).contains("label: \"DataEntity\""),
                        "{language} persistence: PRODUCERS disagrees with the DataEntity facts of {}",
                        adapter_crate(language)
                    ),
                    Family::EndpointAuth => assert_eq!(
                        claimed,
                        auth.contains(*language),
                        "{language} endpoint auth: PRODUCERS disagrees with the RECOVERY languages \
                         in spec/src/workbench/endpoint_permissions.rs"
                    ),
                }
            }
        }
        for language in &auth {
            assert!(
                PRODUCERS.iter().any(|(listed, _)| listed == language),
                "{language} recovers endpoint auth but is not in PRODUCERS"
            );
        }
    }

    fn detected(language: &str, files: u64, adapter: Option<&str>) -> LanguageDetection {
        LanguageDetection {
            language: language.into(),
            files,
            adapter: adapter.map(str::to_string),
            coverage: Vec::new(),
        }
    }

    // AC-1306: a language with a family missing states it; one with every
    // family, an unlisted language and an uncovered one state nothing.
    #[test]
    fn missing_families_are_named_per_language() {
        let found = missing(&[
            detected("Kotlin", 4, Some("t0.adapter-kotlin")),
            detected("Python", 9, Some("t0.adapter-python")),
            detected("Vue", 2, Some("t0.adapter-ts")),
            detected("Ruby", 3, None),
        ]);
        assert_eq!(found.len(), 1, "{found:?}");
        let kotlin = &found[0];
        assert_eq!(kotlin.capability, "extractor-not-run:endpoint-auth:Kotlin");
        assert_eq!(kotlin.occurrences, 4);
        assert_eq!(kotlin.first_site, "t0.adapter-kotlin");
        assert!(
            kotlin.reason.contains(
                "no installed producer recovers endpoint authentication guards for Kotlin"
            ),
            "{}",
            kotlin.reason
        );
        assert!(missing(&[detected("Python", 9, Some("t0.adapter-python"))]).is_empty());
    }
}
