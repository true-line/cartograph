//! Citation ids and the fact and evidence index (#1785, ADR-0058, AC-0595):
//! `fact-index.md` and `fact-index.json`.
//!
//! A citation id names a fact, not an occurrence: it is derived from the
//! fact's [`FactKey`] alone, so two artifacts citing one endpoint cite one id
//! and a re-export of the same graph yields the same ids.
//!
//! The index is the union of the subjects of every assertion in the export's
//! artifacts, once each, ordered by citation id. It is built from the
//! assertions the export mode already admitted and never reads the graph, so
//! its size follows the cited facts. It stores spans, not the bytes they
//! cover. The two index artifacts carry no assertions: they do not cite
//! themselves.
//!
//! A synthesized assertion that names no stored fact (a derived flow hop, the
//! invalid-provenance fallback, a journey candidate) is keyed by its own
//! subject and listed with the provenance it already carries. One subject
//! cited with differing provenance keeps every distinct record, each naming
//! the artifacts that carry it; none is chosen over another.

use super::render::{Budget, Literal, MethodLabel, RenderLimit, Text};
use super::{ExportMode, SpecAssertion};
use core_graph::FactKey;
use core_prov::{
    ArtifactLocator, ConfidenceTier, EvidenceBasis, EvidenceRef, ProducingMethod, Provenance, Tier,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

/// Artifact id of `fact-index.md`.
pub(super) const MARKDOWN_ID: &str = "fact-index";
/// Artifact id of `fact-index.json`.
pub(super) const JSON_ID: &str = "fact-index-json";

/// Schema of the `fact-index.json` body.
pub const FACT_INDEX_SCHEMA: &str = "cartograph.fact-index/v1";

const CITATION_DOMAIN: &[u8] = b"cartograph:citation:v1";
/// Hex characters kept from the hash: sixty-four bits, never extended.
const CITATION_HEX: usize = 16;

/// The citation id of a fact (ADR-0058 decision 1): `F-` and the first 16
/// lowercase hex characters of BLAKE3 over the domain tag
/// `cartograph:citation:v1`, a zero byte and the canonical JSON of the key.
/// Provenance, properties, the citing artifact and the position in it are not
/// inputs.
pub fn citation_id(key: &FactKey) -> String {
    struct HashWriter<'a>(&'a mut blake3::Hasher);
    impl std::io::Write for HashWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(CITATION_DOMAIN);
    hasher.update(&[0]);
    serde_json::to_writer(HashWriter(&mut hasher), key).expect("fact key serializes");
    format!("F-{}", &hasher.finalize().to_hex().as_str()[..CITATION_HEX])
}

/// How an assertion's `subject_id` maps onto a [`FactKey`], handed over by the
/// producer that built the assertion. Never serialized: an edge subject is
/// `source label destination` and any of the three may contain spaces, so the
/// string alone cannot be split back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Subject {
    /// The subject id is a node id, or the id of a synthesized subject.
    Node,
    /// The subject id is `source label destination`; these are the byte
    /// lengths of the first two parts.
    Edge {
        /// Byte length of the source node id.
        source: usize,
        /// Byte length of the relation label.
        label: usize,
    },
}

impl Subject {
    /// The shape of a `source label destination` subject.
    pub(crate) fn edge(source: &str, label: &str) -> Self {
        Self::Edge {
            source: source.len(),
            label: label.len(),
        }
    }

    fn key(self, subject_id: &str) -> FactKey {
        match self {
            Self::Node => FactKey::Node {
                id: subject_id.into(),
            },
            Self::Edge { source, label } => {
                let label_start = source + 1;
                let label_end = label_start + label;
                let part = |range: std::ops::Range<usize>| {
                    subject_id
                        .get(range)
                        .expect("edge subject is `source label destination`")
                        .to_owned()
                };
                FactKey::Edge {
                    source: part(0..source),
                    label: part(label_start..label_end),
                    destination: part(label_end + 1..subject_id.len()),
                }
            }
        }
    }
}

/// The complete fact and evidence index of one export: the body of
/// `fact-index.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactIndex {
    /// [`FACT_INDEX_SCHEMA`].
    pub schema: String,
    /// The export mode whose admitted assertions the rows were built from.
    pub mode: ExportMode,
    /// One row per cited fact, ordered by citation id.
    pub rows: Vec<FactIndexRow>,
}

/// One cited fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactIndexRow {
    /// [`citation_id`] of `fact`.
    pub citation_id: String,
    /// The fact's identity.
    pub fact: FactKey,
    /// Every subject kind the citing assertions gave it, sorted: a node
    /// label, an edge label, `FlowHop`, or a synthesized kind.
    pub subject_kinds: Vec<SharedText>,
    /// Ids of the artifacts with an assertion citing this fact, sorted.
    pub cited_by: Vec<SharedText>,
    /// True when the citing assertions do not agree on the evidence record.
    /// Every distinct record is then listed and names its own artifacts.
    #[serde(default, skip_serializing_if = "is_false")]
    pub provenance_differs: bool,
    /// The evidence the fact is cited with: one record, or every distinct
    /// one when `provenance_differs`.
    pub records: Vec<FactRecord>,
}

/// The evidence one or more citing assertions carry for a fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactRecord {
    /// Producing tier.
    pub tier: Tier,
    /// Confidence tier.
    pub confidence: ConfidenceTier,
    /// True for an unresolved fact: Gap confidence, or a `Gap` node
    /// (R-INT-2, R-INT-4).
    #[serde(default, skip_serializing_if = "is_false")]
    pub gap: bool,
    /// True for an InferredWeak record. Only a `best-effort` index holds one
    /// (R-INT-5).
    #[serde(default, skip_serializing_if = "is_false")]
    pub inferred_weak: bool,
    /// Producing method.
    pub method: ProducingMethod,
    /// Evidence basis: the recorded one, or the tier's default.
    pub evidence_basis: EvidenceBasis,
    /// The extractor that produced the fact.
    pub extractor_id: SharedText,
    /// Every evidence span: repository, path, byte range and commit.
    pub evidence: Vec<FactSpan>,
    /// The artifacts citing the fact with this record, sorted. Listed only
    /// when the row's `provenance_differs`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cited_by: Vec<SharedText>,
}

/// A string a fact index holds many times over (an artifact id, a subject
/// kind, an extractor, a repository, a path or a commit), shared rather than
/// copied into every row (#2215). It serializes, compares and prints as the
/// string it holds.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SharedText(std::sync::Arc<str>);

impl SharedText {
    /// The text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for SharedText {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for SharedText {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SharedText {
    fn from(text: &str) -> Self {
        Self(text.into())
    }
}

impl PartialEq<str> for SharedText {
    fn eq(&self, other: &str) -> bool {
        *self.0 == *other
    }
}

impl PartialEq<&str> for SharedText {
    fn eq(&self, other: &&str) -> bool {
        *self.0 == **other
    }
}

impl PartialEq<String> for SharedText {
    fn eq(&self, other: &String) -> bool {
        *self.0 == **other
    }
}

impl fmt::Debug for SharedText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, f)
    }
}

impl fmt::Display for SharedText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for SharedText {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SharedText {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|text| Self(text.into()))
    }
}

/// One evidence span of a [`FactRecord`]: an [`EvidenceRef`] whose
/// repository, path and commit are shared across the index's rows. It
/// serializes as the [`EvidenceRef`] it stands for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactSpan {
    /// Repository the evidence lives in (e.g. `owner/name`).
    pub repo: SharedText,
    /// Path within the repository.
    pub path: SharedText,
    /// Byte span start (inclusive).
    pub byte_start: u64,
    /// Byte span end (exclusive).
    pub byte_end: u64,
    /// Commit the span was read at.
    pub commit_sha: SharedText,
}

impl PartialEq<EvidenceRef> for FactSpan {
    fn eq(&self, other: &EvidenceRef) -> bool {
        self.repo == other.repo
            && self.path == other.path
            && self.byte_start == other.byte_start
            && self.byte_end == other.byte_end
            && self.commit_sha == other.commit_sha
    }
}

/// The one shared copy of each repeated string a [`Collector`] holds.
#[derive(Default)]
struct Interner(std::collections::HashSet<SharedText>);

impl Interner {
    fn get(&mut self, text: &str) -> SharedText {
        if let Some(held) = self.0.get(text) {
            return held.clone();
        }
        let held = SharedText::from(text);
        self.0.insert(held.clone());
        held
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl FactRecord {
    fn of(provenance: &Provenance, shared: &mut Interner) -> Self {
        Self {
            tier: provenance.tier,
            confidence: provenance.confidence_tier,
            gap: false,
            inferred_weak: false,
            method: provenance.method.clone(),
            evidence_basis: provenance.basis(),
            extractor_id: shared.get(&provenance.extractor_id),
            evidence: provenance
                .evidence
                .iter()
                .map(|span| FactSpan {
                    repo: shared.get(&span.repo),
                    path: shared.get(&span.path),
                    byte_start: span.byte_start,
                    byte_end: span.byte_end,
                    commit_sha: shared.get(&span.commit_sha),
                })
                .collect(),
            cited_by: Vec::new(),
        }
    }

    /// Whether `provenance` is this record. The content hash is not part of
    /// a row, so it is not compared.
    fn is(&self, provenance: &Provenance) -> bool {
        self.tier == provenance.tier
            && self.confidence == provenance.confidence_tier
            && self.method == provenance.method
            && self.extractor_id == provenance.extractor_id
            && self.evidence.len() == provenance.evidence.len()
            && self
                .evidence
                .iter()
                .zip(&provenance.evidence)
                .all(|(a, b)| a == b)
            && self.evidence_basis == provenance.basis()
    }
}

/// Why an export's citations do not resolve (ADR-0058 decisions 3, 5, 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactIndexError {
    /// A cited id has no row in the index.
    Unresolved {
        /// The cited id.
        citation_id: String,
    },
    /// Two different facts have one citation id. The id is never lengthened
    /// to tell them apart, so the export fails.
    Collision {
        /// The shared id.
        citation_id: String,
        /// The fact that held the id first.
        first: Box<FactKey>,
        /// The different fact with the same id.
        second: Box<FactKey>,
    },
    /// A `verified-only` export cites an InferredWeak fact, which that mode
    /// gives no row.
    Withheld {
        /// The id of the InferredWeak fact.
        citation_id: String,
    },
}

impl FactIndexError {
    /// The failure without its detail, for a selected-artifact outcome.
    pub fn failure(&self) -> FactIndexFailure {
        match self {
            Self::Unresolved { .. } => FactIndexFailure::Unresolved,
            Self::Collision { .. } => FactIndexFailure::Collision,
            Self::Withheld { .. } => FactIndexFailure::Withheld,
        }
    }
}

impl fmt::Display for FactIndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unresolved { citation_id } => {
                write!(f, "citation {citation_id} has no row in the fact index")
            }
            Self::Collision {
                citation_id,
                first,
                second,
            } => write!(
                f,
                "citation {citation_id} names two different facts: {first:?} and {second:?}"
            ),
            Self::Withheld { citation_id } => write!(
                f,
                "a verified-only export cites the InferredWeak fact {citation_id}"
            ),
        }
    }
}
impl std::error::Error for FactIndexError {}

/// [`FactIndexError`] without its detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactIndexFailure {
    /// See [`FactIndexError::Unresolved`].
    Unresolved,
    /// See [`FactIndexError::Collision`].
    Collision,
    /// See [`FactIndexError::Withheld`].
    Withheld,
}

/// Fail unless every cited id has a row in `index` (ADR-0058 decision 6).
/// Compiling an export runs this over the export's own assertions; an
/// authored section is held to the same function with the ids it cites.
pub fn check_citations<'a>(
    index: &FactIndex,
    cited: impl IntoIterator<Item = &'a str>,
) -> Result<(), FactIndexError> {
    let rows: BTreeSet<&str> = index
        .rows
        .iter()
        .map(|row| row.citation_id.as_str())
        .collect();
    match cited.into_iter().find(|id| !rows.contains(id)) {
        Some(id) => Err(FactIndexError::Unresolved {
            citation_id: id.into(),
        }),
        None => Ok(()),
    }
}

/// Whether the artifact is one of the two index artifacts.
pub(super) fn is_index(artifact_id: &str) -> bool {
    matches!(artifact_id, MARKDOWN_ID | JSON_ID)
}

/// Whether the artifact carries assertions and so can cite a fact. The JSON
/// sidecars and the coverage artifacts carry none, so the index never needs
/// them compiled.
pub(super) fn cites(artifact_id: &str) -> bool {
    !is_index(artifact_id)
        && !matches!(
            artifact_id,
            "us-tm-index"
                | "gap-register-index"
                | "rule-evidence-index"
                | "coverage"
                | "coverage-index"
        )
}

/// Bytes of `text` that both renderings are certain to write.
fn written(text: &str) -> usize {
    text.bytes().filter(|byte| *byte != b'\r').count()
}

fn insert_sorted(values: &mut Vec<SharedText>, value: &str, shared: &mut Interner) {
    if let Err(at) = values.binary_search_by(|have| have.as_str().cmp(value)) {
        reserve_one(values);
        values.insert(at, shared.get(value));
    }
}

/// Room for exactly one more element. Most rows hold one subject kind, one
/// citing artifact and one record, and a `Vec`'s first push reserves four:
/// on REA's 250,000 gap-register rows that unused room alone was hundreds of
/// MB (#2215).
fn reserve_one<T>(values: &mut Vec<T>) {
    if values.len() == values.capacity() {
        values.reserve_exact(1);
    }
}

/// Accumulates the rows of one export from its artifacts' assertions.
pub(super) struct Collector {
    mode: ExportMode,
    id: fn(&FactKey) -> String,
    /// Keyed by citation id. Every cited id gets its row here, so these keys
    /// are exactly the ids the export cites.
    rows: BTreeMap<String, FactIndexRow>,
    lower_bound: usize,
    shared: Interner,
}

impl Collector {
    pub(super) fn new(mode: ExportMode, id: fn(&FactKey) -> String) -> Self {
        Self {
            mode,
            id,
            rows: BTreeMap::new(),
            lower_bound: 0,
            shared: Interner::default(),
        }
    }

    /// Bytes either rendered index is certain to reach: every key, extractor
    /// and span string held so far is written at least once, and neither
    /// rendering shortens a string except by dropping a carriage return.
    pub(super) fn lower_bound(&self) -> usize {
        self.lower_bound
    }

    /// Record every assertion of one artifact as a citation of its subject.
    pub(super) fn cite(
        &mut self,
        artifact_id: &str,
        assertions: &[SpecAssertion],
        subjects: &[Subject],
    ) -> Result<(), FactIndexError> {
        assert_eq!(
            assertions.len(),
            subjects.len(),
            "every assertion of {artifact_id} hands over its subject key"
        );
        for (assertion, subject) in assertions.iter().zip(subjects) {
            let fact = subject.key(&assertion.subject_id);
            let citation_id = (self.id)(&fact);
            let provenance = &assertion.provenance;
            if self.mode == ExportMode::VerifiedOnly
                && provenance.confidence_tier == ConfidenceTier::InferredWeak
            {
                return Err(FactIndexError::Withheld { citation_id });
            }
            // Charged only for what this citation adds to the index.
            let mut charge = 0;
            let row = match self.rows.entry(citation_id) {
                std::collections::btree_map::Entry::Occupied(entry) => {
                    let row = entry.into_mut();
                    if row.fact != fact {
                        return Err(FactIndexError::Collision {
                            citation_id: row.citation_id.clone(),
                            first: Box::new(row.fact.clone()),
                            second: Box::new(fact),
                        });
                    }
                    row
                }
                std::collections::btree_map::Entry::Vacant(entry) => {
                    charge += entry.key().len()
                        + match &fact {
                            FactKey::Node { id } => written(id),
                            FactKey::Edge {
                                source,
                                label,
                                destination,
                            } => written(source) + written(label) + written(destination),
                        };
                    let citation_id = entry.key().clone();
                    entry.insert(FactIndexRow {
                        citation_id,
                        fact,
                        subject_kinds: Vec::new(),
                        cited_by: Vec::new(),
                        provenance_differs: false,
                        records: Vec::new(),
                    })
                }
            };
            insert_sorted(
                &mut row.subject_kinds,
                &assertion.subject_kind,
                &mut self.shared,
            );
            insert_sorted(&mut row.cited_by, artifact_id, &mut self.shared);
            let record = match row.records.iter().position(|r| r.is(provenance)) {
                Some(at) => &mut row.records[at],
                None => {
                    charge += written(&provenance.extractor_id)
                        + provenance
                            .evidence
                            .iter()
                            .map(|e| written(&e.repo) + written(&e.path) + written(&e.commit_sha))
                            .sum::<usize>();
                    reserve_one(&mut row.records);
                    row.records
                        .push(FactRecord::of(provenance, &mut self.shared));
                    row.records.last_mut().expect("record just pushed")
                }
            };
            insert_sorted(&mut record.cited_by, artifact_id, &mut self.shared);
            self.lower_bound = self.lower_bound.saturating_add(charge);
        }
        Ok(())
    }

    /// The finished index. Every id the export cites has a row, because each
    /// citation makes one; the `Result` stays for the callers' error path.
    pub(super) fn finish(self) -> Result<FactIndex, FactIndexError> {
        let rows = self
            .rows
            .into_values()
            .map(|mut row| {
                let gap_node = row.subject_kinds.iter().any(|kind| kind == "Gap");
                row.provenance_differs = row.records.len() > 1;
                for record in &mut row.records {
                    record.gap = gap_node || record.confidence == ConfidenceTier::Gap;
                    record.inferred_weak = record.confidence == ConfidenceTier::InferredWeak;
                    if !row.provenance_differs {
                        // Not printed: release it rather than keep its slot.
                        record.cited_by = Vec::new();
                    }
                }
                if row.provenance_differs {
                    // Order by content, never by which artifact cited first.
                    row.records.sort_by_cached_key(|record| {
                        serde_json::to_string(record).expect("fact record serializes")
                    });
                }
                row
            })
            .collect();
        let index = FactIndex {
            schema: FACT_INDEX_SCHEMA.into(),
            mode: self.mode,
            rows,
        };
        // Every citation made its row, so every cited id resolves.
        Ok(index)
    }
}

struct FactText<'a>(&'a FactKey);
impl fmt::Display for FactText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            FactKey::Node { id } => write!(f, "node {id}"),
            FactKey::Edge {
                source,
                label,
                destination,
            } => write!(f, "edge {source} {label} {destination}"),
        }
    }
}

struct BasisText<'a>(&'a EvidenceBasis);
impl fmt::Display for BasisText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            EvidenceBasis::Source => f.write_str("source"),
            EvidenceBasis::Observation => f.write_str("observation"),
            EvidenceBasis::Artifact { refs } => {
                f.write_str("artifact")?;
                for (i, artifact) in refs.iter().enumerate() {
                    f.write_str(if i == 0 { " " } else { "; " })?;
                    write!(f, "{} {}", artifact.kind, artifact.digest)?;
                    match &artifact.at {
                        ArtifactLocator::Offset { start, end } => {
                            write!(f, " bytes {start}..{end}")?
                        }
                        ArtifactLocator::Member { path } => write!(f, " member {path}")?,
                    }
                }
                Ok(())
            }
        }
    }
}

struct Listed<'a>(&'a [SharedText]);
impl fmt::Display for Listed<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, value) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            f.write_str(value)?;
        }
        Ok(())
    }
}

/// The evidence cell of one record, printed as `render::EvidenceText` prints
/// a provenance's spans (a record holds the spans alone).
struct Spans<'a>(&'a [FactSpan]);
impl fmt::Display for Spans<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("—");
        }
        for (i, e) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(
                f,
                "{}:{} bytes {}..{} @ {}",
                e.repo, e.path, e.byte_start, e.byte_end, e.commit_sha
            )?;
        }
        Ok(())
    }
}

fn mode_name(mode: ExportMode) -> &'static str {
    match mode {
        ExportMode::VerifiedOnly => "verified-only",
        ExportMode::BestEffort => "best-effort",
    }
}

/// `fact-index.md`: the rows of the index as one table. Every cell that
/// holds recovered text is [`Literal`] Markdown, so a stored id, path or
/// extractor name cannot break the table or become markup.
fn markdown(index: &FactIndex, out: &mut Text) -> fmt::Result {
    out.push_str("# Fact and evidence index\n\n")?;
    out.push_str(
        "Every fact that an assertion in this export cites, listed once and ordered by citation \
         id. A citation id names the fact itself, not a place in a document, so it is the same \
         in every artifact and in every export of the same graph. Each row gives the fact's \
         tier, confidence, producing method, evidence basis, extractor and every evidence span. \
         The spans locate the source; the source text is not copied here. `fact-index.json` \
         holds the same rows as data.\n\n",
    )?;
    writeln!(out, "- Export mode: `{}`", mode_name(index.mode))?;
    writeln!(out, "- Cited facts: {}", index.rows.len())?;
    writeln!(
        out,
        "- Gaps (unresolved, never a confirmed fact): {}",
        index
            .rows
            .iter()
            .filter(|row| row.records.iter().any(|record| record.gap))
            .count()
    )?;
    match index.mode {
        ExportMode::VerifiedOnly => out.push_str(
            "- InferredWeak facts: withheld by `verified-only`, so none has a row and no \
             statement cites one.\n",
        )?,
        ExportMode::BestEffort => writeln!(
            out,
            "- InferredWeak facts (marked; absent from a `verified-only` export): {}",
            index
                .rows
                .iter()
                .filter(|row| row.records.iter().any(|record| record.inferred_weak))
                .count()
        )?,
    }
    writeln!(
        out,
        "- Facts cited with differing provenance (every record listed): {}",
        index
            .rows
            .iter()
            .filter(|row| row.provenance_differs)
            .count()
    )?;
    if index.rows.is_empty() {
        return out.push_str("\nNo assertion in this export cites a fact.\n");
    }
    out.push_str(
        "\n| Citation | Fact | Kind | Tier | Confidence | Method | Basis | Extractor | Evidence \
         | Cited by | Note |\n|---|---|---|---|---|---|---|---|---|---|---|\n",
    )?;
    for row in &index.rows {
        for (ordinal, record) in row.records.iter().enumerate() {
            write!(
                out,
                "| {} | {} | {} | {:?} | {:?} | {} | {} | {} | {} | {} | ",
                row.citation_id,
                Literal(FactText(&row.fact)),
                Literal(Listed(&row.subject_kinds)),
                record.tier,
                record.confidence,
                Literal(MethodLabel(&record.method)),
                Literal(BasisText(&record.evidence_basis)),
                Literal(&record.extractor_id),
                Literal(Spans(&record.evidence)),
                Literal(Listed(&row.cited_by)),
            )?;
            let mut noted = false;
            let mut note = |out: &mut Text, text: fmt::Arguments<'_>| -> fmt::Result {
                if noted {
                    out.push_str("; ")?;
                }
                noted = true;
                out.write_fmt(text)
            };
            if record.gap {
                note(out, format_args!("Gap: unresolved"))?;
            }
            if record.inferred_weak {
                note(out, format_args!("InferredWeak: best-effort only"))?;
            }
            if row.provenance_differs {
                note(
                    out,
                    format_args!(
                        "differing provenance: record {} of {}, as cited by {}",
                        ordinal + 1,
                        row.records.len(),
                        Literal(Listed(&record.cited_by))
                    ),
                )?;
            }
            if !noted {
                out.push_str("—")?;
            }
            out.push_str(" |\n")?;
        }
    }
    Ok(())
}

/// `fact-index.md` (`as_markdown`) or `fact-index.json` for a finished index,
/// refused whole when it would exceed `max`.
pub(super) fn render(
    index: &FactIndex,
    as_markdown: bool,
    max: Option<usize>,
) -> Result<String, RenderLimit> {
    let budget = Budget::new(max)?;
    let mut content = Text::new(&budget, "")?;
    if as_markdown {
        markdown(index, &mut content)?;
    } else {
        serde_json::to_writer(&mut content, index).map_err(|_| RenderLimit)?;
    }
    Ok(content.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SelectedArtifactLimit, SelectedArtifactOutcome, SelectedCompileLimits, SpecBundle,
        compile_spec, compile_spec_selected,
    };
    use core_graph::{Edge, Node};
    use flowtracer::Flow;

    fn span(path: &str, start: u64, end: u64) -> EvidenceRef {
        EvidenceRef {
            repo: "local/shop".into(),
            path: path.into(),
            byte_start: start,
            byte_end: end,
            commit_sha: "abc123".into(),
        }
    }

    fn prov(tier: Tier, confidence: ConfidenceTier, at: EvidenceRef, identity: &str) -> Provenance {
        Provenance::new(
            tier,
            confidence,
            vec![at],
            "spec.fact-index.test",
            identity.as_bytes(),
        )
        .unwrap()
    }

    fn node(id: &str, label: &str, tier: Tier, confidence: ConfidenceTier) -> Node {
        Node {
            id: id.into(),
            label: label.into(),
            props: serde_json::json!({
                "name": id,
                "prov": prov(tier, confidence, span("src/app.ts", 1, 9), id),
            }),
        }
    }

    fn edge(src: &str, label: &str, dst: &str, tier: Tier, confidence: ConfidenceTier) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: label.into(),
            props: serde_json::json!({
                "prov": prov(
                    tier,
                    confidence,
                    span("src/app.ts", 20, 29),
                    &format!("{src} {label} {dst}"),
                ),
            }),
        }
    }

    const ENDPOINT: &str = "ep:local/shop@GET:/orders";

    fn endpoint() -> Node {
        Node {
            id: ENDPOINT.into(),
            label: "Endpoint".into(),
            props: serde_json::json!({
                "method": "GET",
                "path": "/orders",
                "prov": prov(
                    Tier::Deterministic,
                    ConfidenceTier::Confirmed,
                    span("src/routes.ts", 120, 164),
                    ENDPOINT,
                ),
            }),
        }
    }

    /// An endpoint, a decision cited by two artifacts, a topology with one
    /// weak inference and an explicit Gap.
    fn graph() -> (Vec<Node>, Vec<Edge>) {
        use ConfidenceTier as C;
        let nodes = vec![
            endpoint(),
            node(
                "cap:orders",
                "Capability",
                Tier::Deterministic,
                C::Confirmed,
            ),
            node("adr:queue", "ADR", Tier::Semantic, C::InferredStrong),
            node("res:queue", "Resource", Tier::Deterministic, C::Confirmed),
            node("res:worker", "Resource", Tier::Deterministic, C::Confirmed),
            node("res:guess", "Resource", Tier::Agentic, C::InferredWeak),
            node("gap:channel", "Gap", Tier::Deterministic, C::Gap),
        ];
        let edges = vec![
            edge(
                "adr:queue",
                "DECIDES",
                "cap:orders",
                Tier::Semantic,
                C::InferredStrong,
            ),
            edge(
                "res:queue",
                "TRIGGERS",
                "res:worker",
                Tier::Deterministic,
                C::Confirmed,
            ),
            edge(
                "res:worker",
                "REFERENCES",
                "res:guess",
                Tier::Agentic,
                C::InferredWeak,
            ),
        ];
        (nodes, edges)
    }

    fn export(nodes: &[Node], edges: &[Edge], mode: ExportMode) -> SpecBundle {
        let flows: Vec<Flow> = flowtracer::trace(nodes, edges);
        compile_spec(nodes, edges, &flows, mode, &BTreeSet::new())
    }

    fn content<'a>(bundle: &'a SpecBundle, id: &str) -> &'a str {
        &bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == id)
            .unwrap_or_else(|| panic!("{id} is exported"))
            .content
    }

    fn index(bundle: &SpecBundle) -> FactIndex {
        serde_json::from_str(content(bundle, JSON_ID)).expect("fact-index.json parses")
    }

    fn row<'a>(index: &'a FactIndex, fact: &FactKey) -> &'a FactIndexRow {
        let id = citation_id(fact);
        index
            .rows
            .iter()
            .find(|row| row.citation_id == id)
            .unwrap_or_else(|| panic!("{id} has a row"))
    }

    fn node_key(id: &str) -> FactKey {
        FactKey::Node { id: id.into() }
    }

    fn edge_key(source: &str, label: &str, destination: &str) -> FactKey {
        FactKey::Edge {
            source: source.into(),
            label: label.into(),
            destination: destination.into(),
        }
    }

    fn assertion(subject_id: &str, kind: &str, provenance: Provenance) -> SpecAssertion {
        SpecAssertion {
            id: format!("occurrence:{subject_id}"),
            subject_id: subject_id.into(),
            subject_kind: kind.into(),
            summary: "not carried by the index".into(),
            provenance,
            edge_label: None,
            omission: None,
        }
    }

    fn confirmed(identity: &str) -> Provenance {
        prov(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            span("src/app.ts", 1, 9),
            identity,
        )
    }

    // AC-0595: the id is exactly the ADR-0058 hash of the fact key's
    // existing serde form, and nothing else is an input.
    #[test]
    fn citation_id_is_the_adr_0058_hash_of_the_fact_key() {
        let expected = |json: &str| {
            let mut bytes = b"cartograph:citation:v1\0".to_vec();
            bytes.extend_from_slice(json.as_bytes());
            format!("F-{}", &blake3::hash(&bytes).to_hex().as_str()[..16])
        };
        let id = citation_id(&node_key("ep:GET:/orders"));
        assert_eq!(id, expected(r#"{"kind":"node","id":"ep:GET:/orders"}"#));
        assert_eq!(id.len(), 18);
        assert!(
            id[2..]
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        );
        assert_eq!(
            citation_id(&edge_key("sym:a", "CALLS", "sym:b")),
            expected(r#"{"kind":"edge","source":"sym:a","label":"CALLS","destination":"sym:b"}"#)
        );
        // A node whose id reads like an edge is a different fact.
        assert_ne!(
            citation_id(&node_key("sym:a CALLS sym:b")),
            citation_id(&edge_key("sym:a", "CALLS", "sym:b"))
        );
    }

    // AC-0595: an edge subject is recovered from the lengths its producer
    // handed over, never by splitting on spaces that an id may contain.
    #[test]
    fn edge_subject_with_spaces_in_its_ids_keeps_its_exact_key() {
        let (source, label, destination) = ("sym:a CALLS b", "CALLS", "sym:c CALLS d");
        let subject_id = format!("{source} {label} {destination}");
        assert_eq!(
            Subject::edge(source, label).key(&subject_id),
            edge_key(source, label, destination)
        );
        assert_eq!(Subject::Node.key(&subject_id), node_key(&subject_id));
    }

    // AC-0595: a section citing an endpoint resolves to a row with its span.
    #[test]
    fn cited_endpoint_resolves_to_a_row_with_its_span() {
        let (nodes, edges) = graph();
        let bundle = export(&nodes, &edges, ExportMode::VerifiedOnly);
        let mut citing: Vec<&str> = bundle
            .artifacts
            .iter()
            .filter(|artifact| artifact.assertions.iter().any(|a| a.subject_id == ENDPOINT))
            .map(|artifact| artifact.id.as_str())
            .collect();
        assert!(!citing.is_empty(), "a document states the endpoint");
        // A row names its citing artifacts sorted, not in export order.
        citing.sort_unstable();

        let index = index(&bundle);
        let cited = citation_id(&node_key(ENDPOINT));
        check_citations(&index, [cited.as_str()]).expect("the citation resolves");
        let row = row(&index, &node_key(ENDPOINT));
        assert_eq!(row.fact, node_key(ENDPOINT));
        assert_eq!(row.subject_kinds, ["Endpoint"]);
        assert_eq!(row.cited_by, citing);
        assert!(!row.provenance_differs);
        let [record] = row.records.as_slice() else {
            panic!("one evidence record: {row:?}");
        };
        assert_eq!(record.tier, Tier::Deterministic);
        assert_eq!(record.confidence, ConfidenceTier::Confirmed);
        assert_eq!(record.method, ProducingMethod::Unrecorded);
        assert_eq!(record.evidence_basis, EvidenceBasis::Source);
        assert_eq!(record.extractor_id, "spec.fact-index.test");
        assert_eq!(record.evidence, [span("src/routes.ts", 120, 164)]);
        assert!(!record.gap && !record.inferred_weak);

        // The Markdown holds the same row: the span, and no source text.
        let line = content(&bundle, MARKDOWN_ID)
            .lines()
            .find(|line| line.starts_with(&format!("| {cited} |")))
            .expect("the endpoint's table row");
        assert!(
            line.contains("local/shop:src/routes.ts bytes 120..164 @ abc123"),
            "{line}"
        );
        assert!(line.contains("| Deterministic | Confirmed | not recorded | source |"));
    }

    // AC-0595: with the fact gone, or for an id nothing has, the check fails.
    #[test]
    fn removed_fact_or_unknown_citation_fails_the_check() {
        let (nodes, edges) = graph();
        let cited = citation_id(&node_key(ENDPOINT));
        let without: Vec<Node> = nodes.into_iter().filter(|n| n.id != ENDPOINT).collect();
        let index = index(&export(&without, &edges, ExportMode::VerifiedOnly));
        assert_eq!(
            check_citations(&index, [cited.as_str()]),
            Err(FactIndexError::Unresolved {
                citation_id: cited.clone()
            })
        );
        // The remaining citations still resolve; one unknown id fails them.
        let known: Vec<&str> = index.rows.iter().map(|r| r.citation_id.as_str()).collect();
        assert!(!known.is_empty());
        check_citations(&index, known.iter().copied()).unwrap();
        let error = check_citations(&index, known.into_iter().chain(["F-0000000000000000"]))
            .expect_err("an id with no row");
        assert_eq!(error.failure(), FactIndexFailure::Unresolved);
        assert!(error.to_string().contains("F-0000000000000000"));
    }

    // AC-0595 (R-INT-5, R-INT-2, R-INT-4): the index follows the export mode
    // and labels what is not confirmed.
    #[test]
    fn verified_only_index_has_no_inferred_weak_row() {
        let (nodes, edges) = graph();
        let weak_node = node_key("res:guess");
        let weak_edge = edge_key("res:worker", "REFERENCES", "res:guess");

        let best = export(&nodes, &edges, ExportMode::BestEffort);
        let best_index = index(&best);
        assert_eq!(best_index.mode, ExportMode::BestEffort);
        for fact in [&weak_node, &weak_edge] {
            let row = row(&best_index, fact);
            assert!(row.records.iter().all(|r| r.inferred_weak), "{row:?}");
            let line = format!("| {} |", row.citation_id);
            assert!(
                content(&best, MARKDOWN_ID)
                    .lines()
                    .any(|l| l.starts_with(&line) && l.contains("InferredWeak: best-effort only"))
            );
        }

        let verified = export(&nodes, &edges, ExportMode::VerifiedOnly);
        let verified_index = index(&verified);
        assert_eq!(verified_index.mode, ExportMode::VerifiedOnly);
        assert!(!verified_index.rows.is_empty());
        for row in &verified_index.rows {
            assert!(
                row.records
                    .iter()
                    .all(|r| !r.inferred_weak && r.confidence != ConfidenceTier::InferredWeak),
                "{row:?}"
            );
        }
        for fact in [&weak_node, &weak_edge] {
            let id = citation_id(fact);
            assert!(check_citations(&verified_index, [id.as_str()]).is_err());
        }
        for artifact in [MARKDOWN_ID, JSON_ID] {
            assert!(!content(&verified, artifact).contains("res:guess"));
        }
        // Every row verified-only keeps is unchanged in best-effort.
        for row in &verified_index.rows {
            assert!(best_index.rows.contains(row), "{row:?}");
        }

        // A Gap is a row like any other, labelled as a Gap, in both modes.
        for (bundle, index) in [(&best, &best_index), (&verified, &verified_index)] {
            let gap = row(index, &node_key("gap:channel"));
            assert!(gap.records.iter().all(|r| r.gap), "{gap:?}");
            let line = format!("| {} |", gap.citation_id);
            assert!(
                content(bundle, MARKDOWN_ID)
                    .lines()
                    .any(|l| l.starts_with(&line) && l.contains("Gap: unresolved"))
            );
        }

        // A weak statement that reached a verified-only export would fail
        // it instead of being listed or silently dropped.
        let mut collector = Collector::new(ExportMode::VerifiedOnly, citation_id);
        let weak = prov(
            Tier::Agentic,
            ConfidenceTier::InferredWeak,
            span("src/app.ts", 1, 9),
            "res:guess",
        );
        let error = collector
            .cite(
                "topology",
                &[assertion("res:guess", "Resource", weak)],
                &[Subject::Node],
            )
            .expect_err("withheld by the mode");
        assert_eq!(error.failure(), FactIndexFailure::Withheld);
    }

    // AC-0595: the same graph yields the same bytes.
    #[test]
    fn two_exports_have_byte_identical_fact_indexes() {
        let (nodes, edges) = graph();
        let (mut reversed_nodes, mut reversed_edges) = (nodes.clone(), edges.clone());
        reversed_nodes.reverse();
        reversed_edges.reverse();
        for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
            let first = export(&nodes, &edges, mode);
            let second = export(&nodes, &edges, mode);
            let reordered = export(&reversed_nodes, &reversed_edges, mode);
            for id in [MARKDOWN_ID, JSON_ID] {
                assert_eq!(content(&first, id), content(&second, id), "{id}");
                assert_eq!(content(&first, id), content(&reordered, id), "{id}");
            }
            let artifact = |bundle: &SpecBundle, id: &str| {
                bundle.artifacts.iter().find(|a| a.id == id).cloned()
            };
            for id in [MARKDOWN_ID, JSON_ID] {
                assert_eq!(artifact(&first, id), artifact(&second, id));
                let index = artifact(&first, id).unwrap();
                assert!(index.verify_identity());
                // The index does not cite itself.
                assert!(index.assertions.is_empty());
            }
            // Rows are ordered by citation id, once each.
            let ids: Vec<String> = index(&first)
                .rows
                .into_iter()
                .map(|row| row.citation_id)
                .collect();
            let mut sorted = ids.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(ids, sorted);
            assert_eq!(index(&first).schema, FACT_INDEX_SCHEMA);
        }
    }

    // AC-0595: one fact cited by two artifacts has one row listing both.
    #[test]
    fn fact_cited_by_two_artifacts_has_one_row_listing_both() {
        let (nodes, edges) = graph();
        let bundle = export(&nodes, &edges, ExportMode::VerifiedOnly);
        let decision = "adr:queue DECIDES cap:orders";
        let citing: Vec<&str> = bundle
            .artifacts
            .iter()
            .filter(|artifact| artifact.assertions.iter().any(|a| a.subject_id == decision))
            .map(|artifact| artifact.id.as_str())
            .collect();
        assert_eq!(citing, ["us-tm", "adrs"]);

        let index = index(&bundle);
        let fact = edge_key("adr:queue", "DECIDES", "cap:orders");
        let id = citation_id(&fact);
        assert_eq!(
            index.rows.iter().filter(|row| row.fact == fact).count(),
            1,
            "one row for one fact"
        );
        let row = row(&index, &fact);
        assert_eq!(row.cited_by, ["adrs", "us-tm"], "sorted artifact ids");
        assert_eq!(row.subject_kinds, ["DECIDES"]);
        assert_eq!(row.records.len(), 1);
        assert!(!row.provenance_differs);
        assert!(row.records[0].cited_by.is_empty());
        assert_eq!(
            content(&bundle, MARKDOWN_ID)
                .lines()
                .filter(|line| line.starts_with(&format!("| {id} |")))
                .count(),
            1
        );

        // Every assertion of every artifact resolves, and every row is cited.
        let total: usize = bundle.artifacts.iter().map(|a| a.assertions.len()).sum();
        assert_eq!(total, bundle.assertion_count);
        assert!(
            index.rows.len() < total,
            "an index of facts, not occurrences"
        );
        for artifact in &bundle.artifacts {
            assert_eq!(
                artifact.assertions.is_empty(),
                !cites(&artifact.id)
                    || !index
                        .rows
                        .iter()
                        .any(|row| row.cited_by.iter().any(|by| *by == artifact.id)),
                "{}",
                artifact.id
            );
        }
    }

    // AC-0595: the id and the row depend on the fact alone, not on the
    // order of artifacts or of assertions within one.
    #[test]
    fn citation_id_is_stable_when_artifact_or_assertion_order_changes() {
        let endpoint = assertion(ENDPOINT, "Endpoint", confirmed(ENDPOINT));
        let call = assertion("sym:a CALLS sym:b", "CALLS", confirmed("sym:a CALLS sym:b"));
        let hop = assertion(
            "sym:a CALLS sym:b",
            "FlowHop",
            confirmed("sym:a CALLS sym:b"),
        );
        let call_shape = Subject::edge("sym:a", "CALLS");
        let build = |artifacts: &[(&str, Vec<(&SpecAssertion, Subject)>)]| {
            let mut collector = Collector::new(ExportMode::BestEffort, citation_id);
            for (artifact, cited) in artifacts {
                let (assertions, subjects): (Vec<SpecAssertion>, Vec<Subject>) = cited
                    .iter()
                    .map(|(assertion, subject)| ((*assertion).clone(), *subject))
                    .unzip();
                collector.cite(artifact, &assertions, &subjects).unwrap();
            }
            collector.finish().unwrap()
        };
        let forward = build(&[
            (
                "test-arrangements",
                vec![(&endpoint, Subject::Node), (&call, call_shape)],
            ),
            ("flow-dossiers", vec![(&hop, call_shape)]),
        ]);
        let backward = build(&[
            ("flow-dossiers", vec![(&hop, call_shape)]),
            (
                "test-arrangements",
                vec![(&call, call_shape), (&endpoint, Subject::Node)],
            ),
        ]);
        assert_eq!(forward, backward);
        for markdown in [true, false] {
            assert_eq!(
                render(&forward, markdown, None).unwrap(),
                render(&backward, markdown, None).unwrap()
            );
        }
        assert_eq!(forward.rows.len(), 2);
        let edge_row = row(&forward, &edge_key("sym:a", "CALLS", "sym:b"));
        assert_eq!(edge_row.subject_kinds, ["CALLS", "FlowHop"]);
        assert_eq!(edge_row.cited_by, ["flow-dossiers", "test-arrangements"]);
        assert_eq!(
            row(&forward, &node_key(ENDPOINT)).citation_id,
            citation_id(&node_key(ENDPOINT))
        );

        // The occurrence id, summary and content hash are not inputs either.
        let mut moved = endpoint.clone();
        moved.id = "another occurrence".into();
        moved.summary = "another sentence".into();
        moved.provenance.content_hash = "0".repeat(64);
        let restated = build(&[("test-arrangements", vec![(&moved, Subject::Node)])]);
        let original = build(&[("test-arrangements", vec![(&endpoint, Subject::Node)])]);
        assert_eq!(restated, original);
    }

    // AC-0595: two different facts with one citation id fail the export;
    // the id is never lengthened to tell them apart.
    #[test]
    fn forced_citation_id_collision_fails_the_export() {
        fn collide(_: &FactKey) -> String {
            "F-0000000000000000".into()
        }
        let (nodes, edges) = graph();
        let flows = flowtracer::trace(&nodes, &edges);
        let none = BTreeSet::new();
        let mode = ExportMode::BestEffort;
        let projection =
            super::super::selected::project(&nodes, &edges, &flows, mode, &none, None).unwrap();
        let filtered_nodes = super::super::filter_nodes(&projection.nodes, mode, &none);
        let filtered_edges = super::super::filter_edges(&projection.edges, mode, &none);
        let compile = |id: fn(&FactKey) -> String| {
            super::super::selected::bundle_with_citation_ids(
                &filtered_nodes,
                &filtered_edges,
                &projection.flows,
                (mode, &none, Default::default()),
                id,
            )
        };
        // The real id function exports the same graph.
        assert_eq!(
            compile(citation_id).as_ref(),
            Ok(&compile_spec(&nodes, &edges, &flows, mode, &none))
        );
        let error = compile(collide).expect_err("no bundle for a collision");
        let FactIndexError::Collision {
            citation_id: shared,
            first,
            second,
        } = &error
        else {
            panic!("{error:?}");
        };
        assert_eq!(shared, "F-0000000000000000");
        assert_ne!(first, second);
        assert_eq!(error.failure(), FactIndexFailure::Collision);
        assert!(error.to_string().contains("two different facts"));

        // One fact cited twice under one id is not a collision.
        let mut collector = Collector::new(mode, collide);
        let cited = [assertion(ENDPOINT, "Endpoint", confirmed(ENDPOINT))];
        collector
            .cite("topology", &cited, &[Subject::Node])
            .unwrap();
        collector.cite("adrs", &cited, &[Subject::Node]).unwrap();
        assert_eq!(collector.finish().unwrap().rows.len(), 1);
    }

    // AC-1211 (T-1211): the strings that repeat across rows (subject kind,
    // citing artifact, extractor, repository, path, commit) are one shared
    // copy, and the index and its JSON read as owned strings would.
    #[test]
    fn repeated_row_strings_are_one_shared_copy() {
        let mut collector = Collector::new(ExportMode::BestEffort, citation_id);
        let cited = [
            assertion("res:a", "Endpoint", confirmed("res:a")),
            assertion("res:b", "Endpoint", confirmed("res:b")),
        ];
        collector
            .cite("topology", &cited, &[Subject::Node, Subject::Node])
            .unwrap();
        let rows: Vec<_> = collector.rows.values().collect();
        let [a, b] = rows.as_slice() else {
            panic!("two rows: {rows:?}");
        };
        let same = |x: &SharedText, y: &SharedText| std::sync::Arc::ptr_eq(&x.0, &y.0);
        assert!(same(&a.subject_kinds[0], &b.subject_kinds[0]));
        assert!(same(&a.cited_by[0], &b.cited_by[0]));
        assert!(same(&a.records[0].cited_by[0], &b.cited_by[0]));
        let (ra, rb) = (&a.records[0], &b.records[0]);
        assert!(same(&ra.extractor_id, &rb.extractor_id));
        let (sa, sb) = (&ra.evidence[0], &rb.evidence[0]);
        assert!(same(&sa.repo, &sb.repo) && same(&sa.path, &sb.path));
        assert!(same(&sa.commit_sha, &sb.commit_sha));
        assert_eq!(ra.evidence, [span("src/app.ts", 1, 9)]);
        let index = collector.finish().unwrap();
        let json = serde_json::to_value(&index).unwrap();
        let row = &json["rows"][0];
        assert_eq!(row["cited_by"], serde_json::json!(["topology"]));
        assert_eq!(row["subject_kinds"], serde_json::json!(["Endpoint"]));
        assert_eq!(
            row["records"][0]["evidence"][0],
            serde_json::to_value(span("src/app.ts", 1, 9)).unwrap()
        );
        let read: FactIndex = serde_json::from_value(json).unwrap();
        assert_eq!(read, index);
    }

    // AC-1206: a row holds no spare room. Most rows have one subject kind,
    // one citing artifact and one record; a second citation grows each by
    // exactly what it adds (#2215).
    #[test]
    fn collected_rows_hold_no_spare_capacity() {
        let mut collector = Collector::new(ExportMode::BestEffort, citation_id);
        let cited = [assertion(ENDPOINT, "Endpoint", confirmed(ENDPOINT))];
        collector
            .cite("topology", &cited, &[Subject::Node])
            .unwrap();
        let row = collector.rows.values().next().unwrap();
        assert_eq!(row.subject_kinds.capacity(), 1);
        assert_eq!(row.cited_by.capacity(), 1);
        assert_eq!(row.records.capacity(), 1);
        assert_eq!(row.records[0].cited_by.capacity(), 1);
        collector.cite("adrs", &cited, &[Subject::Node]).unwrap();
        let row = collector.rows.values().next().unwrap();
        assert_eq!(row.cited_by, ["adrs", "topology"]);
        assert_eq!(row.cited_by.capacity(), 2);
        assert_eq!(row.records.capacity(), 1);
        let index = collector.finish().unwrap();
        assert_eq!(index.rows.len(), 1);
        assert_eq!(index.rows[0].cited_by, ["adrs", "topology"]);
        assert_eq!(index.rows[0].records[0].cited_by.capacity(), 0);
    }

    // AC-0595: one subject cited with differing provenance keeps every
    // record and says which artifacts carry which; none is chosen.
    #[test]
    fn differing_provenance_for_one_fact_lists_every_record() {
        // An edge with no valid provenance is given a Gap placeholder by
        // each reader: the flow tracer's for the hop, the compiler's for
        // the edge. Both are cited under the edge's own key.
        let handler = node(
            "sym:handler",
            "Symbol",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        let unproven = Edge {
            src: ENDPOINT.into(),
            dst: "sym:handler".into(),
            label: "HANDLES".into(),
            props: serde_json::json!({}),
        };
        let bundle = export(
            &[endpoint(), handler],
            std::slice::from_ref(&unproven),
            ExportMode::VerifiedOnly,
        );
        let index = index(&bundle);
        let row = row(&index, &edge_key(ENDPOINT, "HANDLES", "sym:handler"));
        assert!(row.provenance_differs, "{row:?}");
        let extractors: Vec<&str> = row
            .records
            .iter()
            .map(|r| r.extractor_id.as_str())
            .collect();
        assert_eq!(
            extractors,
            ["flowtracer.invalid-provenance", "spec.invalid-provenance"]
        );
        for record in &row.records {
            assert!(record.gap, "a placeholder stays a Gap: {record:?}");
            assert!(record.evidence.is_empty(), "no borrowed evidence");
            assert!(!record.cited_by.is_empty());
            assert!(record.cited_by.iter().all(|id| row.cited_by.contains(id)));
        }
        assert!(
            row.records[0]
                .cited_by
                .iter()
                .any(|by| *by == "flow-dossiers")
        );
        let lines: Vec<&str> = content(&bundle, MARKDOWN_ID)
            .lines()
            .filter(|line| line.starts_with(&format!("| {} |", row.citation_id)))
            .collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("differing provenance: record 1 of 2, as cited by"));
        assert!(lines[1].contains("differing provenance: record 2 of 2, as cited by"));

        // The fallback's content hash differs by reader; it is not part of a
        // row, so two placeholders that agree on everything else are one.
        let mut collector = Collector::new(ExportMode::VerifiedOnly, citation_id);
        for (artifact, identity) in [("topology", "node:res:x"), ("user-stories", "res:x")] {
            let fallback = super::super::fallback_provenance(identity);
            collector
                .cite(
                    artifact,
                    &[assertion("res:x", "Resource", fallback)],
                    &[Subject::Node],
                )
                .unwrap();
        }
        let merged = collector.finish().unwrap();
        assert_eq!(merged.rows.len(), 1);
        assert!(!merged.rows[0].provenance_differs);
        assert_eq!(merged.rows[0].cited_by, ["topology", "user-stories"]);
    }

    // AC-0595 (#1582): the index obeys the selected compiler's output budget
    // and equals the bundle's whatever else is selected.
    #[test]
    fn selected_fact_index_equals_the_bundle_and_obeys_the_output_budget() {
        let (nodes, edges) = graph();
        let flows = flowtracer::trace(&nodes, &edges);
        let none = BTreeSet::new();
        let mode = ExportMode::BestEffort;
        let full = compile_spec(&nodes, &edges, &flows, mode, &none);
        let artifact = |id: &str| full.artifacts.iter().find(|a| a.id == id).unwrap().clone();
        let select = |ids: &[&str], max_bytes: usize| {
            compile_spec_selected(
                &nodes,
                &edges,
                &flows,
                mode,
                &none,
                ids,
                SelectedCompileLimits {
                    max_bytes,
                    adr: crate::AdrLimits {
                        max_rows: usize::MAX,
                        max_output_bytes: usize::MAX,
                    },
                },
            )
            .unwrap()
        };
        // Budget for the two index artifacts alone (content plus `[]`).
        let size = |id: &str| artifact(id).content.len() + 2;
        let both = size(MARKDOWN_ID) + size(JSON_ID);
        assert_eq!(
            select(&[JSON_ID, MARKDOWN_ID], both),
            [
                SelectedArtifactOutcome::Produced(artifact(MARKDOWN_ID)),
                SelectedArtifactOutcome::Produced(artifact(JSON_ID)),
            ]
        );
        // A citing artifact past the budget is refused; the index still
        // lists its citations, because they belong to the export.
        assert!(size("topology") > 2);
        assert_eq!(
            select(&["topology", MARKDOWN_ID], size(MARKDOWN_ID)),
            [
                SelectedArtifactOutcome::Limited {
                    artifact_id: "topology".into(),
                    limit: SelectedArtifactLimit::OutputBytes {
                        remaining: size(MARKDOWN_ID)
                    },
                },
                SelectedArtifactOutcome::Produced(artifact(MARKDOWN_ID)),
            ]
        );
        // One byte short, the index is refused whole, never shortened.
        for max in [size(JSON_ID) - 1, 64, 0] {
            assert_eq!(
                select(&[JSON_ID], max),
                [SelectedArtifactOutcome::Limited {
                    artifact_id: JSON_ID.into(),
                    limit: SelectedArtifactLimit::OutputBytes { remaining: max },
                }]
            );
        }
        // What an earlier artifact used is not available to the index.
        let topology = artifact("topology");
        let used = topology.content.len() + serde_json::to_vec(&topology.assertions).unwrap().len();
        assert_eq!(
            select(&["topology", JSON_ID], used + size(JSON_ID) - 1),
            [
                SelectedArtifactOutcome::Produced(topology),
                SelectedArtifactOutcome::Limited {
                    artifact_id: JSON_ID.into(),
                    limit: SelectedArtifactLimit::OutputBytes {
                        remaining: size(JSON_ID) - 1
                    },
                },
            ]
        );
    }

    // AC-0595: recovered text in a row cannot break the table or become
    // markup, and the JSON keeps the exact key.
    #[test]
    fn fact_index_markdown_keeps_recovered_text_inert() {
        let hostile = "res:a|b\n| F-ffffffffffffffff | <script>`x`</script> ~~y~~";
        let mut collector = Collector::new(ExportMode::BestEffort, citation_id);
        let mut provenance = confirmed(hostile);
        provenance.extractor_id = "x<b>`|~~e~~".into();
        collector
            .cite(
                "topology",
                &[assertion(hostile, "Resource", provenance)],
                &[Subject::Node],
            )
            .unwrap();
        let index = collector.finish().unwrap();
        let markdown = render(&index, true, None).unwrap();
        let rows: Vec<&str> = markdown.lines().filter(|l| l.starts_with("| F-")).collect();
        assert_eq!(rows.len(), 1, "{markdown}");
        assert_eq!(
            rows[0].matches(" | ").count(),
            10,
            "eleven cells: {}",
            rows[0]
        );
        assert!(!markdown.contains("<script>"));
        assert!(!markdown.contains("`x`"));
        assert!(!markdown.contains("~~y~~"));
        let parsed: FactIndex =
            serde_json::from_str(&render(&index, false, None).unwrap()).unwrap();
        assert_eq!(parsed, index);
        assert_eq!(parsed.rows[0].fact, node_key(hostile));
    }

    // AC-0595: a record's spans print exactly as every other artifact
    // prints a provenance's spans.
    #[test]
    fn fact_index_spans_print_as_the_other_artifacts_print_them() {
        let spans = |provenance: &Provenance| {
            let record = FactRecord::of(provenance, &mut Interner::default());
            Spans(&record.evidence).to_string()
        };
        let mut provenance = confirmed("res:x");
        assert_eq!(
            spans(&provenance),
            super::super::render::EvidenceText(&provenance).to_string()
        );
        provenance.evidence.push(span("src/other.ts", 3, 4));
        assert_eq!(
            spans(&provenance),
            super::super::render::EvidenceText(&provenance).to_string()
        );
        provenance.evidence.clear();
        assert_eq!(
            spans(&provenance),
            super::super::render::EvidenceText(&provenance).to_string()
        );
    }
}
