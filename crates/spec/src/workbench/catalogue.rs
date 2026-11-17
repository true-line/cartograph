//! The catalogue section table (#1748, SPEC-15 slice 1, ADR-0064 decision 2,
//! AC-0663).
//!
//! `docs/DOCUMENT-CATALOGUE.md` decides which documents and sections exist.
//! This table is that decision as data, for the catalogue version it names:
//! a section the table does not hold cannot be addressed, compiled or
//! drafted. Version 1 of the table holds D1 only; each further document is a
//! table change. A test reads the catalogue file and fails when the version
//! or a D1 row differs from this table.

use serde::Serialize;

/// The catalogue version this table was written against.
pub const CATALOGUE_VERSION: &str = "1.0";

/// How a section's content is produced (catalogue §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionKind {
    /// P: compiled from facts. No model.
    Projected,
    /// A: prose a role drafts from admitted facts.
    Authored,
    /// P+A: a projected frame, then drafted prose.
    Framed,
    /// H: only a human can supply it.
    Human,
}

impl SectionKind {
    /// The catalogue's own mark for the kind.
    pub fn mark(self) -> &'static str {
        match self {
            Self::Projected => "P",
            Self::Authored => "A",
            Self::Framed => "P+A",
            Self::Human => "H",
        }
    }

    /// Whether a draft can target a section of this kind.
    pub fn drafted(self) -> bool {
        matches!(self, Self::Authored | Self::Framed)
    }
}

/// Where the projected frame of a P or P+A section comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "frame", content = "of", rename_all = "snake_case")]
pub enum SectionFrame {
    /// Every admitted non-Gap node with this label, each cited.
    Nodes(&'static str),
    /// The traced flows, summarized; `flow_dossiers.md` holds each hop.
    Flows,
    /// The Gap findings, counted; `gap_register.md` lists each.
    Gaps,
    /// The statements stay in these artifacts of the same export and are
    /// not repeated in the document.
    Artifacts(&'static [&'static str]),
}

/// One section of one catalogue document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CatalogueSection {
    /// `D1`..`D11`.
    pub document: &'static str,
    /// The catalogue number, such as `1.3`.
    pub number: &'static str,
    /// The catalogue title.
    pub title: &'static str,
    /// P, A, P+A or H.
    pub kind: SectionKind,
    /// The node label of the subject the section is written per, when it is
    /// written per subject. A draft for such a section may be scoped to one
    /// subject by its citation id (ADR-0064 decision 2).
    pub subject: Option<&'static str>,
    /// The projected frame. Present exactly for P and P+A sections.
    pub frame: Option<SectionFrame>,
}

const SECTIONS: &[CatalogueSection] = &[
    CatalogueSection {
        document: "D1",
        number: "1.1",
        title: "Purpose and scope of the system",
        kind: SectionKind::Authored,
        subject: None,
        frame: None,
    },
    CatalogueSection {
        document: "D1",
        number: "1.2",
        title: "Actors and roles",
        kind: SectionKind::Framed,
        subject: Some("Actor"),
        frame: Some(SectionFrame::Nodes("Actor")),
    },
    CatalogueSection {
        document: "D1",
        number: "1.3",
        title: "Capabilities",
        kind: SectionKind::Framed,
        subject: Some("Capability"),
        frame: Some(SectionFrame::Nodes("Capability")),
    },
    CatalogueSection {
        document: "D1",
        number: "1.4",
        title: "Acceptance criteria per capability",
        kind: SectionKind::Framed,
        subject: Some("Capability"),
        frame: Some(SectionFrame::Artifacts(&[
            "rule-evidence.md",
            "test_arrangements.md",
        ])),
    },
    CatalogueSection {
        document: "D1",
        number: "1.5",
        title: "Business rules",
        kind: SectionKind::Framed,
        subject: Some("BusinessRule"),
        // A rule's text is rendered only by the rule inventory, under its own
        // disclosure policy, so the frame is that artifact and never a
        // second rendering of the rule.
        frame: Some(SectionFrame::Artifacts(&["rule-evidence.md"])),
    },
    CatalogueSection {
        document: "D1",
        number: "1.6",
        title: "Business flows",
        kind: SectionKind::Projected,
        subject: None,
        frame: Some(SectionFrame::Flows),
    },
    CatalogueSection {
        document: "D1",
        number: "1.7",
        title: "Out of scope and non-goals",
        kind: SectionKind::Human,
        subject: None,
        frame: None,
    },
    CatalogueSection {
        document: "D1",
        number: "1.8",
        title: "Known gaps",
        kind: SectionKind::Projected,
        subject: None,
        frame: Some(SectionFrame::Gaps),
    },
];

/// The sections of `document` in catalogue order. Empty for a document the
/// table does not hold.
pub fn catalogue_sections(document: &str) -> impl Iterator<Item = &'static CatalogueSection> {
    SECTIONS
        .iter()
        .filter(move |section| section.document == document)
}

/// The section at `(document, number)`, or none: a section the table does
/// not hold cannot be addressed.
pub fn catalogue_section(document: &str, number: &str) -> Option<&'static CatalogueSection> {
    SECTIONS
        .iter()
        .find(|section| section.document == document && section.number == number)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CATALOGUE: &str = include_str!("../../../../docs/DOCUMENT-CATALOGUE.md");

    /// The rows of the catalogue's own D1 table: number, title, kind mark.
    fn catalogue_rows(heading: &str) -> Vec<(String, String, String)> {
        CATALOGUE
            .lines()
            .skip_while(|line| !line.starts_with(heading))
            .skip(1)
            .take_while(|line| !line.starts_with("### "))
            .filter(|line| line.starts_with("| ") && !line.starts_with("| #"))
            .map(|line| {
                let cells: Vec<_> = line.split('|').map(str::trim).collect();
                (cells[1].into(), cells[2].into(), cells[3].into())
            })
            .collect()
    }

    #[test]
    fn table_matches_the_catalogue_version_it_names() {
        assert!(
            CATALOGUE.contains(&format!("**Status:** Version {CATALOGUE_VERSION}.")),
            "the catalogue is no longer version {CATALOGUE_VERSION}"
        );
        let table: Vec<_> = catalogue_sections("D1")
            .map(|s| (s.number.into(), s.title.into(), s.kind.mark().into()))
            .collect();
        assert_eq!(table, catalogue_rows("### D1 "));
        assert_eq!(table.len(), 8);
    }

    #[test]
    fn frame_is_present_exactly_for_projected_and_framed_sections() {
        for section in SECTIONS {
            let framed = matches!(section.kind, SectionKind::Projected | SectionKind::Framed);
            assert_eq!(section.frame.is_some(), framed, "{}", section.number);
            assert_eq!(
                section.kind.drafted(),
                matches!(section.kind, SectionKind::Authored | SectionKind::Framed)
            );
            // Only a drafted section is written per subject.
            assert!(section.subject.is_none() || section.kind.drafted());
        }
    }

    #[test]
    fn section_outside_the_table_cannot_be_addressed() {
        assert_eq!(
            catalogue_section("D1", "1.3").unwrap().title,
            "Capabilities"
        );
        assert!(catalogue_section("D1", "1.9").is_none());
        assert!(catalogue_section("D1", "2.1").is_none());
        // D2 is in the catalogue and not yet in the table.
        assert!(catalogue_section("D2", "2.1").is_none());
        assert_eq!(catalogue_sections("D2").count(), 0);
        assert!(catalogue_section("d1", "1.3").is_none());
    }
}
