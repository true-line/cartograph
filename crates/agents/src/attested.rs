//! Attested context: records that answer a Gap or a section address with
//! what engineers and agents attest, never with recovered evidence
//! (#2463, ADR-0090 decisions 1 to 6 and 8, AC-1071).
//!
//! A record holds one statement for one [`AttestedTarget`], the facts it
//! points at, the content hash each had when the record was made, and its
//! [`Attestation`]s. Every attestation names its contributor by
//! [`ContributorId`] only. A display name or handle lives in the project's
//! contributor table (AC-1073) and is never part of a record, so a record
//! carries no personal detail and several engineers later need only new
//! contributor rows.
//!
//! **Identity.** [`identity`] covers the target, the statement, the
//! supporting citations and their recorded hashes: what is attested. The
//! attestations are reviews of that content and are not inputs, so the
//! [`citation_id`] (`A-` and 16 hex characters) a drafter cites stays the
//! same while engineers confirm, dispute or withdraw.
//!
//! **Standing.** [`standing`] reads the attestations only. The graph states
//! of ADR-0090 decision 8 (`already-covered`, `unresolvable`, `stale`) are
//! applied by the curated projection (AC-1075) before `eligible` and
//! `agent-attested, unconfirmed`, which come from here.
//!
//! [`admit`] is pure: it reads no store and calls no model.

use crate::section_draft::{MAX_CITATIONS, MAX_STATEMENT_BYTES, SectionAddress};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Most attestations one record holds.
pub const MAX_ATTESTATIONS: usize = 64;
/// Most UTF-8 bytes in one free-text attestation field (role, model, run…).
pub const MAX_FIELD_BYTES: usize = 256;
/// Prefix of an attested citation id.
pub const CITATION_PREFIX: &str = "A-";
/// Prefix of a contributor id.
pub const CONTRIBUTOR_PREFIX: &str = "C-";

const IDENTITY_DOMAIN: &[u8] = b"cartograph:attested:v1";
const HEX: usize = 16;

/// An opaque contributor id: `C-` and 16 lowercase hex characters. It
/// resolves to a person or an agent only through the project's contributor
/// table.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContributorId(pub String);

impl ContributorId {
    /// Whether the id has the `C-` form.
    pub fn well_formed(&self) -> bool {
        self.0
            .strip_prefix(CONTRIBUTOR_PREFIX)
            .is_some_and(|hex| hex.len() == HEX && hex.bytes().all(is_lower_hex))
    }
}

/// What a record answers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AttestedTarget {
    /// A Gap, by its id and slot (SPEC-12).
    Gap {
        /// The Gap id.
        gap_id: String,
        /// The missing relation the record answers.
        slot: String,
    },
    /// A section of a catalogue document (SPEC-15).
    Section {
        /// The address.
        address: SectionAddress,
    },
}

/// What an engineer did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineerVerb {
    /// The engineer made the statement.
    Stated,
    /// The engineer confirmed a statement another contributor made.
    Confirmed,
    /// The engineer disputes it.
    Disputed,
    /// The engineer takes back an earlier statement or confirmation.
    Withdrew,
}

/// What an agent did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentVerb {
    /// The agent made the statement.
    Stated,
    /// The agent agrees with a statement another contributor made.
    Agreed,
    /// The agent disputes it.
    Disputed,
}

/// Where an engineer's attestation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttestationSource {
    /// An investigation or an agent chat.
    Investigation,
    /// The Gaps register.
    GapsRegister,
    /// The Spec Workbench.
    Workbench,
}

/// One contributor's attestation, in the order it was recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
pub enum Attestation {
    /// An agent's attestation.
    Agent {
        /// The agent's contributor id.
        contributor: ContributorId,
        /// What it did.
        verb: AgentVerb,
        /// The role, such as `section-writer@1`.
        role: String,
        /// The role definition's fingerprint.
        definition_fingerprint: String,
        /// The provider.
        provider: String,
        /// The model.
        model: String,
        /// The producing run or chat.
        run: String,
        /// The engineer who directed it, when one did. Agents filling a gap
        /// with other agents name none.
        directed_by: Option<ContributorId>,
    },
    /// An engineer's attestation.
    Engineer {
        /// The engineer's contributor id.
        contributor: ContributorId,
        /// What they did.
        verb: EngineerVerb,
        /// Milliseconds since the Unix epoch.
        at_ms: i64,
        /// Where it came from.
        source: AttestationSource,
        /// The conversation or investigation, when there is one.
        reference: Option<String>,
    },
}

impl Attestation {
    /// The contributor.
    pub fn contributor(&self) -> &ContributorId {
        match self {
            Self::Agent { contributor, .. } | Self::Engineer { contributor, .. } => contributor,
        }
    }

    fn states(&self) -> bool {
        matches!(
            self,
            Self::Agent {
                verb: AgentVerb::Stated,
                ..
            } | Self::Engineer {
                verb: EngineerVerb::Stated,
                ..
            }
        )
    }
}

/// An attested record before or after admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttestedRecord {
    /// What it answers.
    pub target: AttestedTarget,
    /// The statement, as plain text.
    pub statement: String,
    /// ADR-0058 citation ids it points at, in the order given. May be empty.
    pub supporting: Vec<String>,
    /// Citation id to the content hash it had when the record was made, for
    /// every supporting id.
    pub recorded_hashes: BTreeMap<String, String>,
    /// The attestations, oldest first.
    pub attestations: Vec<Attestation>,
}

/// How the attestations stand, before graph states apply (ADR-0090
/// decision 8). The variants are listed in the order they win.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    /// The latest engineer attestation is `withdrew`.
    Withdrawn,
    /// A contributor's latest attestation disputes the statement.
    Disputed,
    /// An engineer's latest attestation states or confirms it.
    EngineerBacked,
    /// Only agents attest it: shown as `agent-attested, unconfirmed`, never
    /// accepted until an engineer states or confirms it.
    AgentAttestedUnconfirmed,
}

impl Standing {
    /// The label every surface prints.
    pub fn label(self) -> &'static str {
        match self {
            Self::Withdrawn => "withdrawn",
            Self::Disputed => "disputed",
            Self::EngineerBacked => "attested",
            Self::AgentAttestedUnconfirmed => "agent-attested, unconfirmed",
        }
    }

    /// Whether the record is accepted: only an engineer accepts it.
    pub fn accepted(self) -> bool {
        self == Self::EngineerBacked
    }

    /// Whether a drafter may be admitted the record (ADR-0090 decision 6).
    pub fn citable(self) -> bool {
        matches!(self, Self::EngineerBacked | Self::AgentAttestedUnconfirmed)
    }
}

/// Why a record was refused. Closed: a refusal carries a code and a
/// position, never text from the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum AttestationRefusal {
    /// A part of the target is blank, too long or not plain text.
    TargetIncomplete,
    /// The statement is blank.
    BlankStatement,
    /// The statement is longer than [`MAX_STATEMENT_BYTES`].
    StatementTooLong,
    /// The statement holds a control character.
    StatementNotPlainText,
    /// The statement holds a recognized secret.
    StatementHoldsSecret,
    /// More than [`MAX_CITATIONS`] supporting ids.
    TooManyCitations,
    /// A supporting id is given twice.
    DuplicateCitation,
    /// A supporting id is not an `F-` id.
    CitationMalformed,
    /// The recorded hashes are not exactly the supporting ids.
    RecordedHashesMismatch,
    /// A recorded hash is not 64 lowercase hex characters (BLAKE3).
    RecordedHashMalformed,
    /// No attestation.
    NoAttestation,
    /// More than [`MAX_ATTESTATIONS`] attestations.
    TooManyAttestations,
    /// The first attestation does not state the statement.
    FirstDoesNotState,
    /// A contributor id is not of the `C-` form.
    ContributorMalformed {
        /// Zero-based position of the attestation.
        attestation: usize,
    },
    /// One contributor id is used both as an agent and as an engineer.
    ContributorKindConflict {
        /// Zero-based position of the attestation.
        attestation: usize,
    },
    /// An agent attestation leaves a field blank, too long or not plain.
    AgentFieldInvalid {
        /// Zero-based position of the attestation.
        attestation: usize,
    },
    /// An agent names a directing contributor that is not an engineer of
    /// this record, or itself.
    DirectorNotEngineer {
        /// Zero-based position of the attestation.
        attestation: usize,
    },
    /// An engineer's reference is too long or not plain text.
    ReferenceInvalid {
        /// Zero-based position of the attestation.
        attestation: usize,
    },
}

impl AttestationRefusal {
    /// The reason code, as serialized.
    pub fn code(self) -> &'static str {
        match self {
            Self::TargetIncomplete => "target_incomplete",
            Self::BlankStatement => "blank_statement",
            Self::StatementTooLong => "statement_too_long",
            Self::StatementNotPlainText => "statement_not_plain_text",
            Self::StatementHoldsSecret => "statement_holds_secret",
            Self::TooManyCitations => "too_many_citations",
            Self::DuplicateCitation => "duplicate_citation",
            Self::CitationMalformed => "citation_malformed",
            Self::RecordedHashesMismatch => "recorded_hashes_mismatch",
            Self::RecordedHashMalformed => "recorded_hash_malformed",
            Self::NoAttestation => "no_attestation",
            Self::TooManyAttestations => "too_many_attestations",
            Self::FirstDoesNotState => "first_does_not_state",
            Self::ContributorMalformed { .. } => "contributor_malformed",
            Self::ContributorKindConflict { .. } => "contributor_kind_conflict",
            Self::AgentFieldInvalid { .. } => "agent_field_invalid",
            Self::DirectorNotEngineer { .. } => "director_not_engineer",
            Self::ReferenceInvalid { .. } => "reference_invalid",
        }
    }
}

/// Admit `record`, or refuse it whole with the first failing rule.
pub fn admit(record: AttestedRecord) -> Result<AttestedRecord, AttestationRefusal> {
    use AttestationRefusal as R;
    let complete = match &record.target {
        AttestedTarget::Gap { gap_id, slot } => [gap_id, slot]
            .into_iter()
            .all(|part| field_admissible(part)),
        AttestedTarget::Section { address } => [
            &address.catalogue_version,
            &address.document,
            &address.section,
            &address.scope,
        ]
        .into_iter()
        .all(|part| field_admissible(part)),
    };
    if !complete {
        return Err(R::TargetIncomplete);
    }
    let statement = record.statement.as_str();
    if statement.len() > MAX_STATEMENT_BYTES {
        return Err(R::StatementTooLong);
    }
    if !statement.chars().any(char::is_alphanumeric) {
        return Err(R::BlankStatement);
    }
    if statement.chars().any(char::is_control) {
        return Err(R::StatementNotPlainText);
    }
    if !core_redact::recognized_secrets(statement).is_empty() {
        return Err(R::StatementHoldsSecret);
    }
    if record.supporting.len() > MAX_CITATIONS {
        return Err(R::TooManyCitations);
    }
    let mut once = BTreeSet::new();
    for id in &record.supporting {
        if !once.insert(id.as_str()) {
            return Err(R::DuplicateCitation);
        }
        if !fact_id_well_formed(id) {
            return Err(R::CitationMalformed);
        }
    }
    if record.recorded_hashes.len() != once.len()
        || !record
            .recorded_hashes
            .keys()
            .all(|id| once.contains(id.as_str()))
    {
        return Err(R::RecordedHashesMismatch);
    }
    if !record
        .recorded_hashes
        .values()
        .all(|hash| content_hash_shaped(hash))
    {
        return Err(R::RecordedHashMalformed);
    }
    if record.attestations.is_empty() {
        return Err(R::NoAttestation);
    }
    if record.attestations.len() > MAX_ATTESTATIONS {
        return Err(R::TooManyAttestations);
    }
    if !record.attestations[0].states() {
        return Err(R::FirstDoesNotState);
    }
    let mut engineers = BTreeSet::new();
    let mut agents = BTreeSet::new();
    for (attestation, value) in record.attestations.iter().enumerate() {
        let id = value.contributor();
        if !id.well_formed() {
            return Err(R::ContributorMalformed { attestation });
        }
        let (mine, other) = match value {
            Attestation::Engineer { .. } => (&mut engineers, &agents),
            Attestation::Agent { .. } => (&mut agents, &engineers),
        };
        if other.contains(id) {
            return Err(R::ContributorKindConflict { attestation });
        }
        mine.insert(id.clone());
    }
    for (attestation, value) in record.attestations.iter().enumerate() {
        match value {
            Attestation::Agent {
                contributor,
                role,
                definition_fingerprint,
                provider,
                model,
                run,
                directed_by,
                ..
            } => {
                if ![role, definition_fingerprint, provider, model, run]
                    .into_iter()
                    .all(|field| field_admissible(field))
                {
                    return Err(R::AgentFieldInvalid { attestation });
                }
                if let Some(director) = directed_by
                    && (director == contributor || !engineers.contains(director))
                {
                    return Err(R::DirectorNotEngineer { attestation });
                }
            }
            Attestation::Engineer { reference, .. } => {
                if let Some(reference) = reference
                    && !field_admissible(reference)
                {
                    return Err(R::ReferenceInvalid { attestation });
                }
            }
        }
    }
    Ok(record)
}

/// How `attestations` stand: each contributor's latest attestation counts.
/// Withdrawn wins when the latest engineer attestation is `withdrew`, then
/// any latest dispute, then an engineer's statement or confirmation; with
/// none of those the record is agent-attested and unconfirmed.
pub fn standing(attestations: &[Attestation]) -> Standing {
    let mut latest: BTreeMap<&ContributorId, &Attestation> = BTreeMap::new();
    let mut last_engineer = None;
    for value in attestations {
        latest.insert(value.contributor(), value);
        if let Attestation::Engineer { verb, .. } = value {
            last_engineer = Some(*verb);
        }
    }
    if last_engineer == Some(EngineerVerb::Withdrew) {
        return Standing::Withdrawn;
    }
    let mut backed = false;
    for value in latest.values() {
        match value {
            Attestation::Agent {
                verb: AgentVerb::Disputed,
                ..
            }
            | Attestation::Engineer {
                verb: EngineerVerb::Disputed,
                ..
            } => return Standing::Disputed,
            Attestation::Engineer {
                verb: EngineerVerb::Stated | EngineerVerb::Confirmed,
                ..
            } => backed = true,
            _ => {}
        }
    }
    if backed {
        Standing::EngineerBacked
    } else {
        Standing::AgentAttestedUnconfirmed
    }
}

/// The record's identity: BLAKE3 over the domain tag, a zero byte and the
/// canonical JSON of its target, statement, supporting ids and recorded
/// hashes. Attestations are not inputs.
pub fn identity(record: &AttestedRecord) -> String {
    #[derive(Serialize)]
    struct Content<'a> {
        target: &'a AttestedTarget,
        statement: &'a str,
        supporting: &'a [String],
        recorded_hashes: &'a BTreeMap<String, String>,
    }
    let mut bytes = IDENTITY_DOMAIN.to_vec();
    bytes.push(0);
    serde_json::to_writer(
        &mut bytes,
        &Content {
            target: &record.target,
            statement: &record.statement,
            supporting: &record.supporting,
            recorded_hashes: &record.recorded_hashes,
        },
    )
    .expect("attested content serializes");
    core_prov::content_hash(&bytes)
}

/// The record's citation id: `A-` and the first 16 hex characters of its
/// [`identity`].
pub fn citation_id(record: &AttestedRecord) -> String {
    format!("{CITATION_PREFIX}{}", &identity(record)[..HEX])
}

/// Whether `id` has the `A-` form.
pub fn citation_id_well_formed(id: &str) -> bool {
    id.strip_prefix(CITATION_PREFIX)
        .is_some_and(|hex| hex.len() == HEX && hex.bytes().all(is_lower_hex))
}

fn fact_id_well_formed(id: &str) -> bool {
    id.strip_prefix("F-")
        .is_some_and(|hex| hex.len() == HEX && hex.bytes().all(is_lower_hex))
}

fn content_hash_shaped(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(is_lower_hex)
}

fn field_admissible(field: &str) -> bool {
    field.len() <= MAX_FIELD_BYTES
        && field.chars().any(char::is_alphanumeric)
        && !field.chars().any(char::is_control)
}

fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

#[cfg(test)]
#[path = "attested_tests.rs"]
mod tests;
