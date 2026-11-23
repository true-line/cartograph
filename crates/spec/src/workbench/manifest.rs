//! The bundle manifest (#1752, AC-0682, SPEC-16): `bundle-manifest.md`.
//!
//! One file that states what a rebuild handoff bundle was compiled from and
//! which exported file holds each of its parts. It is a deterministic
//! listing: no graph fact is read, no model is called and nothing is
//! asserted about the system, so it carries no assertion and no citation.
//!
//! **As-of.** The catalogue version, the export mode, the graph content
//! hash, the curated revision and the source revisions, each as the host
//! supplied it. A value the host did not supply is stated as not supplied;
//! none is invented or computed here.
//!
//! **Parts.** Every file of the official export is listed once, under the
//! SPEC-16 part it belongs to. A file no part names is listed under its own
//! heading, so a file is never left out of the manifest.
//!
//! **Cost.** Linear in the artifact catalogue and the source revisions.

use super::catalogue::CATALOGUE_VERSION;
use super::producers::Output;
use super::render::{Budget, Literal, RenderLimit, Text};
use super::{Curated, ExportMode};
use crate::SourceRevision;
use std::fmt::Write as _;

/// The artifact id.
pub(crate) const ARTIFACT_ID: &str = "bundle-manifest";

const INTRO: &str = "# Bundle manifest\n\n\
What this export was compiled from and which file holds each part of the rebuild handoff \
bundle (SPEC-16). It lists the files of a full export; it asserts nothing about the system and \
cites no fact. A value the host did not supply is stated as not supplied.\n\n";

/// One SPEC-16 part: its title, where its content is when it has no file of
/// its own, and the artifacts that hold it.
struct Part {
    title: &'static str,
    note: &'static str,
    ids: &'static [&'static str],
}

const PARTS: &[Part] = &[
    Part {
        title: "1. Documents",
        note: "D1 is `product-specification.md`. The content of D2 to D10 is under the \
               current artifact names, mapped in the document catalogue.",
        ids: &[
            "d1-product-specification",
            "user-stories",
            "flow-dossiers",
            "journeys",
            "topology",
            "data-model",
            "adrs",
            "drift-register",
            "security-view",
            "endpoint-permissions",
            "toolchain",
            "test-arrangements",
            "pipelines",
            "configuration",
            "external",
            "environments",
            "documentation-evidence",
            "endpoint-inventory",
            "curated",
            "architecture",
            "architecture-dsl",
        ],
    },
    Part {
        title: "2. Requirements",
        note: "Sections 1.3 and 1.4 of `product-specification.md`. No file of its own.",
        ids: &[],
    },
    Part {
        title: "3. Rules",
        note: "Section 1.5 of `product-specification.md`, with the evidence in these files.",
        ids: &["rule-evidence", "rule-evidence-index"],
    },
    Part {
        title: "4. Traceability matrix",
        note: "",
        ids: &["us-tm", "us-tm-index"],
    },
    Part {
        title: "5. Gap register",
        note: "The coverage files state what was read and what was skipped.",
        ids: &[
            "gap-register",
            "gap-register-index",
            "coverage",
            "coverage-index",
        ],
    },
    Part {
        title: "6. Fact index",
        note: "",
        ids: &["fact-index", "fact-index-json"],
    },
    Part {
        title: "7. Work packets",
        note: "Not written by the export. Each packet is generated on request and exported \
               as its own file; a packet is a proposal and grants no authority.",
        ids: &[],
    },
    Part {
        title: "8. As-of",
        note: "",
        ids: &[ARTIFACT_ID],
    },
];

/// A registry key or a revision. One made only of identifier characters is
/// printed as it is stored, so a reader can copy it; any other is printed as
/// escaped literal text, so a stored value cannot start markup or a cell.
struct Identifier<'a>(&'a str);
impl std::fmt::Display for Identifier<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let plain = !self.0.is_empty()
            && self
                .0
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._/@:+-".contains(&byte));
        if plain {
            write!(f, "`{}`", self.0)
        } else {
            Literal(self.0).fmt(f)
        }
    }
}

fn mode_name(mode: ExportMode) -> &'static str {
    match mode {
        ExportMode::VerifiedOnly => "verified-only",
        ExportMode::BestEffort => "best-effort",
    }
}

/// What the manifest states the export was compiled from.
#[derive(Clone, Copy)]
pub(super) struct AsOf<'a> {
    pub(super) mode: ExportMode,
    pub(super) curated: Curated<'a>,
    pub(super) graph_content_hash: Option<&'a str>,
    pub(super) source_revisions: Option<&'a [SourceRevision]>,
}

/// `bundle-manifest.md` over the official artifact catalogue, given as
/// `(id, file name)` in compilation order.
pub(super) fn output(
    files: &[(&'static str, &'static str)],
    as_of: AsOf<'_>,
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let mut content = Text::new(&budget, INTRO)?;
    writeln!(
        content,
        "## As-of\n\n\
         - Document catalogue version: {CATALOGUE_VERSION}\n\
         - Export mode: `{}`",
        mode_name(as_of.mode)
    )?;
    // The host's own hash first, as the D1 header does (AC-0663).
    let hash = as_of.graph_content_hash.or(as_of
        .curated
        .projection()
        .map(|projection| projection.graph_basis.as_str()));
    match hash {
        Some(hash) => writeln!(content, "- Graph content hash: {}", Literal(hash))?,
        None => content.push_str("- Graph content hash: not supplied to this compilation\n")?,
    }
    match as_of.curated {
        Curated::Computed(projection) => writeln!(
            content,
            "- Curated revision: {}",
            Literal(&projection.revision)
        )?,
        Curated::NotComputed(failure) => writeln!(
            content,
            "- Curated revision: not computed (`{}`); see `curated.md`",
            failure.code()
        )?,
        Curated::NotSupplied => {
            content.push_str("- Curated revision: not supplied to this compilation\n")?;
        }
    }
    match as_of.source_revisions {
        None => content.push_str("- Source revisions: not supplied to this compilation\n")?,
        Some([]) => content.push_str("- Source revisions: the graph holds no source\n")?,
        Some(sources) => {
            writeln!(content, "- Source revisions: {}", sources.len())?;
            content.push_str("\n| Source | Revision |\n|---|---|\n")?;
            for source in sources {
                writeln!(
                    content,
                    "| {} | {} |",
                    Identifier(&source.repo),
                    Identifier(&source.commit)
                )?;
            }
        }
    }

    writeln!(
        content,
        "\n## Parts\n\nFiles in a full export: {}.",
        files.len()
    )?;
    let mut listed = Vec::new();
    for part in PARTS {
        writeln!(content, "\n### {}\n", part.title)?;
        if !part.note.is_empty() {
            writeln!(content, "{}\n", part.note)?;
        }
        for id in part.ids {
            // A part names an id the catalogue may not hold; only a file the
            // export writes is listed.
            if let Some((_, file)) = files.iter().find(|(known, _)| known == id) {
                writeln!(content, "- `{file}`")?;
                listed.push(*id);
            }
        }
    }
    let unassigned: Vec<_> = files
        .iter()
        .filter(|(id, _)| !listed.contains(id))
        .collect();
    if !unassigned.is_empty() {
        content.push_str("\n### Files no part names\n\n")?;
        for (_, file) in unassigned {
            writeln!(content, "- `{file}`")?;
        }
    }
    Ok(Output {
        content: content.finish(),
        assertions: Vec::new(),
        subjects: Vec::new(),
        attested: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    //! AC-0682 (T-0682): `bundle-manifest.md`.
    use super::*;
    use crate::{
        CuratedFailure, SelectedArtifactLimit, SelectedArtifactOutcome, SelectedCompileLimits,
        SpecArtifact, SpecBundle, Supplied, artifact_catalog, compile_spec,
        compile_spec_selected_supplied, try_compile_spec_supplied,
    };
    use std::collections::BTreeSet;

    const MODES: [ExportMode; 2] = [ExportMode::VerifiedOnly, ExportMode::BestEffort];

    fn sources() -> Vec<SourceRevision> {
        vec![
            SourceRevision {
                repo: "acme/shop".into(),
                commit: "abc123".into(),
            },
            SourceRevision {
                repo: "local/k_3".into(),
                commit: "workdir".into(),
            },
        ]
    }

    fn supplied(sources: Option<&[SourceRevision]>) -> Supplied<'_> {
        Supplied {
            repo_names: &[],
            coverage: &[],
            stale_coverage: &[],
            curated: Curated::NotComputed(CuratedFailure::ReviewsUnavailable),
            graph_content_hash: Some("graph-hash-1"),
            source_revisions: sources,
            knowledge: None,
        }
    }

    fn compile(mode: ExportMode, supplied: Supplied<'_>) -> SpecBundle {
        try_compile_spec_supplied(&[], &[], &[], mode, &BTreeSet::new(), supplied).unwrap()
    }

    fn manifest(bundle: &SpecBundle) -> &SpecArtifact {
        bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == ARTIFACT_ID)
            .unwrap()
    }

    #[test]
    fn the_manifest_states_the_as_of_the_host_supplied() {
        let sources = sources();
        for (mode, name) in MODES.into_iter().zip(["verified-only", "best-effort"]) {
            let bundle = compile(mode, supplied(Some(&sources)));
            let manifest = manifest(&bundle);
            assert_eq!(manifest.file_name, "bundle-manifest.md");
            let content = &manifest.content;
            assert!(content.contains(&format!(
                "- Document catalogue version: {CATALOGUE_VERSION}\n"
            )));
            assert!(content.contains(&format!("- Export mode: `{name}`\n")));
            assert!(content.contains("- Graph content hash: graph-hash-1\n"));
            assert!(content.contains(
                "- Curated revision: not computed (`reviews_unavailable`); see `curated.md`\n"
            ));
            assert!(content.contains("- Source revisions: 2\n"));
            assert!(content.contains("| `acme/shop` | `abc123` |\n| `local/k_3` | `workdir` |\n"));
            // It asserts nothing about the system and cites no fact.
            assert!(manifest.assertions.is_empty());
            assert!(!content.contains("F-"));
        }
    }

    #[test]
    fn a_value_the_host_did_not_supply_is_stated_and_not_invented() {
        let bare = compile_spec(&[], &[], &[], ExportMode::BestEffort, &BTreeSet::new());
        let content = &manifest(&bare).content;
        assert!(content.contains("- Graph content hash: not supplied to this compilation\n"));
        assert!(content.contains("- Curated revision: not supplied to this compilation\n"));
        assert!(content.contains("- Source revisions: not supplied to this compilation\n"));

        // A graph with no source is not the same statement as no supply.
        let empty = compile(ExportMode::BestEffort, supplied(Some(&[])));
        assert!(
            manifest(&empty)
                .content
                .contains("- Source revisions: the graph holds no source\n")
        );
    }

    #[test]
    fn every_exported_file_is_listed_once_under_a_part() {
        let catalog = artifact_catalog();
        let bundle = compile(ExportMode::BestEffort, supplied(None));
        let content = &manifest(&bundle).content;
        assert!(content.contains(&format!("Files in a full export: {}.\n", catalog.len())));
        for artifact in &bundle.artifacts {
            let line = format!("- `{}`\n", artifact.file_name);
            assert_eq!(content.matches(&line).count(), 1, "{}", artifact.file_name);
        }
        // Each part names artifacts the catalogue holds, each once, and no
        // file falls outside the parts.
        let mut named: Vec<_> = PARTS.iter().flat_map(|part| part.ids).copied().collect();
        named.sort_unstable();
        let mut known: Vec<_> = catalog.iter().map(|artifact| artifact.id).collect();
        known.sort_unstable();
        assert_eq!(named, known);
        assert!(!content.contains("### Files no part names"));
        for title in PARTS.iter().map(|part| part.title) {
            assert!(content.contains(&format!("\n### {title}\n")));
        }
    }

    #[test]
    fn a_file_no_part_names_is_still_listed() {
        let files = [("us-tm", "US-TM.md"), ("new-artifact", "new.md")];
        let as_of = AsOf {
            mode: ExportMode::BestEffort,
            curated: Curated::NotSupplied,
            graph_content_hash: None,
            source_revisions: None,
        };
        let content = output(&files, as_of, None).unwrap().content;
        assert!(content.contains("\n### Files no part names\n\n- `new.md`\n"));
        assert_eq!(content.matches("- `US-TM.md`\n").count(), 1);
    }

    #[test]
    fn a_stored_value_is_printed_as_inert_text() {
        let sources = [SourceRevision {
            repo: "acme/<b>shop</b>".into(),
            commit: "a|b\n# heading".into(),
        }];
        let bundle = compile(ExportMode::BestEffort, supplied(Some(&sources)));
        let content = &manifest(&bundle).content;
        assert!(!content.contains("<b>"));
        assert!(!content.contains("\n# heading"));
        assert!(!content.contains("| a|b"));
    }

    #[test]
    fn the_manifest_is_deterministic_selected_alone_and_bounded() {
        let sources = sources();
        let limits = |max_bytes| SelectedCompileLimits {
            max_bytes,
            adr: crate::AdrLimits {
                max_rows: usize::MAX,
                max_output_bytes: usize::MAX,
            },
        };
        let select = |max_bytes| {
            compile_spec_selected_supplied(
                &[],
                &[],
                &[],
                ExportMode::BestEffort,
                &BTreeSet::new(),
                &[ARTIFACT_ID],
                limits(max_bytes),
                supplied(Some(&sources)),
            )
            .unwrap()
        };
        let bundle = compile(ExportMode::BestEffort, supplied(Some(&sources)));
        assert_eq!(
            bundle,
            compile(ExportMode::BestEffort, supplied(Some(&sources)))
        );
        match &select(usize::MAX)[..] {
            [SelectedArtifactOutcome::Produced(alone)] => assert_eq!(alone, manifest(&bundle)),
            other => panic!("{other:?}"),
        }
        match &select(64)[..] {
            [SelectedArtifactOutcome::Limited { artifact_id, limit }] => {
                assert_eq!(artifact_id, ARTIFACT_ID);
                assert!(matches!(limit, SelectedArtifactLimit::OutputBytes { .. }));
            }
            other => panic!("{other:?}"),
        }
    }
}
