//! One capability table for every built-in specialist version (ADR-0071,
//! #1751).
//!
//! Each [`SpecialistId`] has exactly one row, in enum declaration order. A row
//! names the role, the public role version, the private prompt generation and
//! the protocol capabilities that version opts into. Every `uses_*` method on
//! [`SpecialistId`] reads this table; a new behaviour is a new [`Capability`]
//! and the rows that carry it, not a new `match` arm in many places.
//!
//! The rows only describe the immutable definitions. Prompt text and compiled
//! definition data stay in `specialists.rs` and `data/`.

use super::SpecialistId;
use serde::Serialize;

/// The user-facing role a specialist version belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpecialistRole {
    /// Investigates scoped business behaviour, features and design evidence.
    DomainAnalyst,
    /// Audits claim support, coverage limitations and unresolved evidence.
    EvidenceAuditor,
    /// Drafts cited specification narrative.
    SpecWriter,
    /// Triages recorded gaps into cited design notes.
    GapTriager,
    /// Drafts one catalogue section as cited claims.
    SectionWriter,
}

impl SpecialistRole {
    /// Every role, in catalog order.
    pub const ALL: [Self; 5] = [
        Self::DomainAnalyst,
        Self::EvidenceAuditor,
        Self::SpecWriter,
        Self::GapTriager,
        Self::SectionWriter,
    ];

    /// The stable id prefix, for example `domain-analyst` in `domain-analyst@27`.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::DomainAnalyst => "domain-analyst",
            Self::EvidenceAuditor => "evidence-auditor",
            Self::SpecWriter => "spec-writer",
            Self::GapTriager => "gap-triager",
            Self::SectionWriter => "section-writer",
        }
    }
}

/// One protocol behaviour a specialist version opts into. Each variant answers
/// what one `SpecialistId::uses_*` method answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Capability {
    /// Shared compact context inventories instead of per-query pages.
    CompactDisclosure,
    /// Host-derived `turn_progress` in every invocation.
    TurnProgress,
    /// Caller-selected `inspection_requirements` targets (@5 only).
    InspectionRequirements,
    /// Coverage qualification and `read_coverage_inventory`.
    CoverageQualification,
    /// The completion-obligation contract.
    CompletionObligations,
    /// Versioned investigation policy selection.
    PolicySelection,
    /// Fixed query references and charged `replay_query`.
    QueryHistoryDisclosure,
    /// Next-prompt admission and saved run limits.
    NextPromptAdmission,
    /// The schema-2 finish with schema-8 cited narrative.
    CitedNarrative,
    /// Combined finish-feedback receipt sizing and correction.
    CombinedFinishFeedback,
    /// Volatile finish-patch correction.
    FinishPatch,
    /// The selected per-call response schema is shown in the prompt.
    VisibleActionSchema,
    /// Exact source-read reservation.
    ExactSourceReservation,
    /// Typed query-budget refusals.
    QueryBudgetRefusals,
    /// Host-offered acquisition choices.
    AcquisitionChoices,
    /// Exact-node queries.
    ExactNodeQueries,
    /// Removal of actions refused for prompt capacity.
    CapacityExclusions,
    /// The finish-feedback witness.
    FinishFeedbackWitness,
    /// Typed citation correction.
    TypedCitationCorrection,
    /// Host-only finding withholding after one authenticated correction.
    FindingWithholding,
    /// The saved `LocalFinish4096V1` generation for new local desktop starts.
    LocalFinishGeneration,
    /// Each step is one native tool call in a tool-calling runtime (Pi for
    /// local models, the Agent SDK for Claude) instead of one JSON action
    /// (ADR-0085).
    NativeToolRuntime,
    /// `read_artifact`: one page of a compiled catalogue artifact from the
    /// frozen basis, supplied as untrusted context (ADR-0089).
    ArtifactReads,
    /// One bounded drop correction naming unacquired citation ids, in
    /// choice and finish-only modes, under the single repair (#2478).
    UnacquiredCitationDrop,
    /// A read refused for prompt capacity stays withdrawn from the offered
    /// choices for the rest of the run, not only at its own input revision
    /// (#2522).
    RunCapacityExclusions,
}

impl Capability {
    /// Every capability, in declaration order.
    pub const ALL: [Self; 25] = [
        Self::CompactDisclosure,
        Self::TurnProgress,
        Self::InspectionRequirements,
        Self::CoverageQualification,
        Self::CompletionObligations,
        Self::PolicySelection,
        Self::QueryHistoryDisclosure,
        Self::NextPromptAdmission,
        Self::CitedNarrative,
        Self::CombinedFinishFeedback,
        Self::FinishPatch,
        Self::VisibleActionSchema,
        Self::ExactSourceReservation,
        Self::QueryBudgetRefusals,
        Self::AcquisitionChoices,
        Self::ExactNodeQueries,
        Self::CapacityExclusions,
        Self::FinishFeedbackWitness,
        Self::TypedCitationCorrection,
        Self::FindingWithholding,
        Self::LocalFinishGeneration,
        Self::NativeToolRuntime,
        Self::ArtifactReads,
        Self::UnacquiredCitationDrop,
        Self::RunCapacityExclusions,
    ];

    const fn bit(self) -> u32 {
        1 << self as u8
    }
}

/// A fixed set of [`Capability`] values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CapabilitySet(u32);

impl CapabilitySet {
    /// The set holding exactly `capabilities`.
    pub const fn of(capabilities: &[Capability]) -> Self {
        let mut bits = 0;
        let mut index = 0;
        while index < capabilities.len() {
            bits |= capabilities[index].bit();
            index += 1;
        }
        Self(bits)
    }

    /// Whether `capability` is in the set.
    pub const fn contains(self, capability: Capability) -> bool {
        self.0 & capability.bit() != 0
    }

    /// The capabilities in the set, in [`Capability::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = Capability> {
        Capability::ALL
            .into_iter()
            .filter(move |capability| self.contains(*capability))
    }
}

/// Whether a new start may use a specialist version (ADR-0071 decision 1).
///
/// Every version still decodes, so a stored run made with any of them reopens;
/// availability only decides which versions the catalog offers for new work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// The role's current version: the one a role-based start uses.
    Current,
    /// Still offered for a new start, as an explicit version choice.
    Offered,
    /// Kept only so stored runs reopen; no new start may use it.
    Retired,
}

/// One row of the specialist capability table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpecialistRow {
    /// The specialist this row describes.
    pub id: SpecialistId,
    /// The role the version belongs to.
    pub role: SpecialistRole,
    /// The public role version, the number after `@` in the id.
    pub version: u32,
    /// The private prompt protocol generation; independent of the role version.
    pub prompt_generation: u32,
    /// Whether a new start may use this version.
    pub availability: Availability,
    /// The protocol behaviours this version opts into.
    pub capabilities: CapabilitySet,
}

const fn row(
    id: SpecialistId,
    role: SpecialistRole,
    version: u32,
    prompt_generation: u32,
    availability: Availability,
    capabilities: &[Capability],
) -> SpecialistRow {
    SpecialistRow {
        id,
        role,
        version,
        prompt_generation,
        availability,
        capabilities: CapabilitySet::of(capabilities),
    }
}

use Availability as A;
use Capability as C;
use SpecialistId as S;
use SpecialistRole as R;

/// Every built-in specialist version, in `SpecialistId` declaration order:
/// id, role, role version, prompt generation, availability, capabilities.
#[rustfmt::skip]
pub const SPECIALIST_TABLE: [SpecialistRow; 49] = [
    row(S::DomainAnalystV1, R::DomainAnalyst, 1, 1, A::Retired, &[]),
    row(S::EvidenceAuditorV1, R::EvidenceAuditor, 1, 1, A::Retired, &[]),
    row(S::DomainAnalyst, R::DomainAnalyst, 2, 2, A::Retired, &[]),
    row(S::EvidenceAuditor, R::EvidenceAuditor, 2, 2, A::Retired, &[]),
    row(S::DomainAnalystV3, R::DomainAnalyst, 3, 3, A::Retired, &[C::CompactDisclosure]),
    row(S::EvidenceAuditorV3, R::EvidenceAuditor, 3, 3, A::Retired, &[C::CompactDisclosure]),
    row(S::DomainAnalystV4, R::DomainAnalyst, 4, 4, A::Retired, &[C::CompactDisclosure, C::TurnProgress]),
    row(S::EvidenceAuditorV4, R::EvidenceAuditor, 4, 4, A::Retired, &[C::CompactDisclosure, C::TurnProgress]),
    row(S::DomainAnalystV5, R::DomainAnalyst, 5, 5, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::InspectionRequirements]),
    row(S::EvidenceAuditorV5, R::EvidenceAuditor, 5, 5, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::InspectionRequirements]),
    row(S::DomainAnalystV6, R::DomainAnalyst, 6, 6, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification]),
    row(S::EvidenceAuditorV6, R::EvidenceAuditor, 6, 6, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification]),
    row(S::DomainAnalystV7, R::DomainAnalyst, 7, 7, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations]),
    row(S::EvidenceAuditorV7, R::EvidenceAuditor, 7, 7, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations]),
    row(S::DomainAnalystV8, R::DomainAnalyst, 8, 8, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection]),
    row(S::EvidenceAuditorV8, R::EvidenceAuditor, 8, 8, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection]),
    row(S::DomainAnalystV9, R::DomainAnalyst, 9, 9, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure]),
    row(S::EvidenceAuditorV9, R::EvidenceAuditor, 9, 9, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure]),
    row(S::DomainAnalystV10, R::DomainAnalyst, 10, 10, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission]),
    row(S::EvidenceAuditorV10, R::EvidenceAuditor, 10, 10, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission]),
    row(S::DomainAnalystV11, R::DomainAnalyst, 11, 10, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission]),
    row(S::DomainAnalystV12, R::DomainAnalyst, 12, 10, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission]),
    row(S::DomainAnalystV13, R::DomainAnalyst, 13, 10, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission]),
    row(S::DomainAnalystV14, R::DomainAnalyst, 14, 10, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission]),
    row(S::DomainAnalystV15, R::DomainAnalyst, 15, 10, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission]),
    row(S::DomainAnalystV17, R::DomainAnalyst, 17, 10, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::ExactNodeQueries]),
    row(S::DomainAnalystV18, R::DomainAnalyst, 18, 10, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::AcquisitionChoices, C::ExactNodeQueries]),
    row(S::DomainAnalystV19, R::DomainAnalyst, 19, 10, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::AcquisitionChoices, C::ExactNodeQueries]),
    row(S::DomainAnalystV20, R::DomainAnalyst, 20, 10, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::AcquisitionChoices, C::ExactNodeQueries, C::TypedCitationCorrection]),
    row(S::DomainAnalystV21, R::DomainAnalyst, 21, 11, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::AcquisitionChoices, C::ExactNodeQueries, C::CapacityExclusions, C::TypedCitationCorrection]),
    row(S::DomainAnalystV22, R::DomainAnalyst, 22, 12, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::AcquisitionChoices, C::ExactNodeQueries, C::CapacityExclusions, C::FinishFeedbackWitness, C::TypedCitationCorrection]),
    row(S::DomainAnalystV23, R::DomainAnalyst, 23, 13, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::AcquisitionChoices, C::ExactNodeQueries, C::CapacityExclusions, C::FinishFeedbackWitness, C::TypedCitationCorrection]),
    row(S::DomainAnalystV24, R::DomainAnalyst, 24, 14, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::CombinedFinishFeedback, C::ExactSourceReservation, C::AcquisitionChoices, C::ExactNodeQueries, C::CapacityExclusions, C::FinishFeedbackWitness, C::TypedCitationCorrection]),
    row(S::DomainAnalystV25, R::DomainAnalyst, 25, 14, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::CombinedFinishFeedback, C::ExactSourceReservation, C::AcquisitionChoices, C::ExactNodeQueries, C::CapacityExclusions, C::FinishFeedbackWitness, C::TypedCitationCorrection, C::FindingWithholding]),
    row(S::DomainAnalystV26, R::DomainAnalyst, 26, 14, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::CombinedFinishFeedback, C::ExactSourceReservation, C::AcquisitionChoices, C::ExactNodeQueries, C::CapacityExclusions, C::FinishFeedbackWitness, C::TypedCitationCorrection, C::FindingWithholding, C::LocalFinishGeneration]),
    row(S::DomainAnalystV27, R::DomainAnalyst, 27, 14, A::Offered, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::CombinedFinishFeedback, C::FinishPatch, C::ExactSourceReservation, C::AcquisitionChoices, C::ExactNodeQueries, C::CapacityExclusions, C::FinishFeedbackWitness, C::TypedCitationCorrection, C::FindingWithholding, C::LocalFinishGeneration]),
    row(S::DomainAnalystV28, R::DomainAnalyst, 28, 14, A::Current, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::CombinedFinishFeedback, C::FinishPatch, C::VisibleActionSchema, C::ExactSourceReservation, C::AcquisitionChoices, C::ExactNodeQueries, C::CapacityExclusions, C::FinishFeedbackWitness, C::TypedCitationCorrection, C::FindingWithholding, C::LocalFinishGeneration]),
    row(S::DomainAnalystV29, R::DomainAnalyst, 29, 14, A::Offered, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::CombinedFinishFeedback, C::FinishPatch, C::VisibleActionSchema, C::ExactSourceReservation, C::AcquisitionChoices, C::ExactNodeQueries, C::CapacityExclusions, C::FinishFeedbackWitness, C::TypedCitationCorrection, C::FindingWithholding, C::LocalFinishGeneration, C::NativeToolRuntime]),
    row(S::DomainAnalystV30, R::DomainAnalyst, 30, 14, A::Offered, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::CombinedFinishFeedback, C::FinishPatch, C::VisibleActionSchema, C::ExactSourceReservation, C::AcquisitionChoices, C::ExactNodeQueries, C::CapacityExclusions, C::FinishFeedbackWitness, C::TypedCitationCorrection, C::FindingWithholding, C::LocalFinishGeneration, C::NativeToolRuntime, C::ArtifactReads]),
    row(S::DomainAnalystV31, R::DomainAnalyst, 31, 14, A::Offered, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::CombinedFinishFeedback, C::FinishPatch, C::VisibleActionSchema, C::ExactSourceReservation, C::AcquisitionChoices, C::ExactNodeQueries, C::CapacityExclusions, C::FinishFeedbackWitness, C::TypedCitationCorrection, C::FindingWithholding, C::LocalFinishGeneration, C::NativeToolRuntime, C::ArtifactReads, C::UnacquiredCitationDrop, C::RunCapacityExclusions]),
    row(S::EvidenceAuditorV11, R::EvidenceAuditor, 11, 10, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission]),
    row(S::EvidenceAuditorV12, R::EvidenceAuditor, 12, 10, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::QueryBudgetRefusals, C::AcquisitionChoices, C::ExactNodeQueries]),
    row(S::EvidenceAuditorV13, R::EvidenceAuditor, 13, 12, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::QueryBudgetRefusals, C::AcquisitionChoices, C::ExactNodeQueries, C::FinishFeedbackWitness, C::TypedCitationCorrection]),
    row(S::EvidenceAuditorV14, R::EvidenceAuditor, 14, 13, A::Current, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::PolicySelection, C::QueryHistoryDisclosure, C::NextPromptAdmission, C::QueryBudgetRefusals, C::AcquisitionChoices, C::ExactNodeQueries, C::CapacityExclusions, C::FinishFeedbackWitness, C::TypedCitationCorrection]),
    row(S::SpecWriterV1, R::SpecWriter, 1, 7, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations]),
    row(S::SpecWriterV2, R::SpecWriter, 2, 7, A::Current, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::CitedNarrative]),
    row(S::GapTriagerV1, R::GapTriager, 1, 7, A::Retired, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations]),
    row(S::GapTriagerV2, R::GapTriager, 2, 7, A::Current, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations, C::CitedNarrative]),
    row(S::SectionWriterV1, R::SectionWriter, 1, 7, A::Current, &[C::CompactDisclosure, C::TurnProgress, C::CoverageQualification, C::CompletionObligations]),
];

impl SpecialistId {
    /// Every specialist id, in declaration order.
    pub fn all() -> impl Iterator<Item = Self> {
        SPECIALIST_TABLE.iter().map(|row| row.id)
    }

    /// This id's row in [`SPECIALIST_TABLE`].
    pub fn row(self) -> &'static SpecialistRow {
        &SPECIALIST_TABLE[self as usize]
    }

    /// The role this version belongs to.
    pub fn role(self) -> SpecialistRole {
        self.row().role
    }

    /// Every capability this version opts into.
    pub fn capabilities(self) -> CapabilitySet {
        self.row().capabilities
    }

    /// Whether a new start may use this version.
    pub fn availability(self) -> Availability {
        self.row().availability
    }

    /// Whether this version opts into `capability`.
    pub fn has(self, capability: Capability) -> bool {
        self.row().capabilities.contains(capability)
    }
}

/// The versions a new start may use, in table order. Versions that select an
/// investigation policy are offered only while `investigation-v2` is
/// registered. Every start path checks this list (ADR-0071 decision 2).
pub fn offered_specialists() -> Vec<SpecialistId> {
    let expanded = super::InvestigationPolicy::for_profile("investigation-v2").is_some();
    SPECIALIST_TABLE
        .iter()
        .filter(|row| row.availability != Availability::Retired)
        .filter(|row| expanded || !row.capabilities.contains(Capability::PolicySelection))
        .map(|row| row.id)
        .collect()
}

/// The role's current version, if the role has one on offer.
pub fn current_specialist(role: SpecialistRole) -> Option<SpecialistId> {
    offered_specialists()
        .into_iter()
        .find(|id| id.role() == role && id.availability() == Availability::Current)
}

#[cfg(test)]
#[path = "capabilities_parity_tests.rs"]
mod parity_tests;

#[cfg(test)]
#[path = "capabilities_typescript_tests.rs"]
mod typescript_tests;
