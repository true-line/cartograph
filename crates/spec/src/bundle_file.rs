//! The spec bundle file, v2 (ADR-0086, #1582): the file Export bundle
//! saves, with each piece once. Artifact content appears only in `files`,
//! each evidence source (repo and commit) once in `sources`, a
//! `node:`/`edge:` assertion id is derived from its subject, and an assertion
//! two artifacts share is listed once in `assertions` and referenced by
//! index. The file is compact JSON with one entry per line.
//!
//! [`BundleFileV2`] writes it one artifact at a time, so a host can stream a
//! bundle it never holds whole (AC-1212). [`read_bundle_file`] reads a v2
//! file or a v1 file (the AC-0740 shape) back to the same [`SpecBundle`].

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};

use core_graph::omission::Omission;
use core_prov::{ConfidenceTier, EvidenceBasis, EvidenceRef, ProducingMethod, Provenance, Tier};
use serde::{Deserialize, Serialize};

use crate::{CuratedBasis, ExportMode, GraphBasis, SpecArtifact, SpecAssertion, SpecBundle};

/// The `schema` a v2 bundle file opens with. A file without one is v1.
pub const BUNDLE_FILE_SCHEMA_V2: &str = "cartograph.spec-bundle-file/v2";

/// One evidence source in the file's `sources` table.
#[derive(Serialize, Deserialize)]
struct Source<'a> {
    #[serde(borrow)]
    repo: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    commit_sha: std::borrow::Cow<'a, str>,
}

/// An evidence row, its repo and commit by `sources` index.
#[derive(Serialize, Deserialize)]
struct Evidence<'a> {
    source: u32,
    #[serde(borrow)]
    path: std::borrow::Cow<'a, str>,
    byte_start: u64,
    byte_end: u64,
}

/// [`Provenance`] with its evidence rows indexed. The field order and the
/// omitted fields are [`Provenance`]'s.
#[derive(Serialize, Deserialize)]
struct FileProvenance<'a> {
    tier: Tier,
    confidence_tier: ConfidenceTier,
    #[serde(borrow)]
    evidence: Vec<Evidence<'a>>,
    #[serde(borrow)]
    extractor_id: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    content_hash: std::borrow::Cow<'a, str>,
    #[serde(default, skip_serializing_if = "ProducingMethod::is_unrecorded")]
    method: ProducingMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    basis: Option<EvidenceBasis>,
}

/// Which prefix a derived assertion id has.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Ref {
    Node,
    Edge,
}

impl Ref {
    fn prefix(self) -> &'static str {
        match self {
            Self::Node => "node:",
            Self::Edge => "edge:",
        }
    }
}

/// [`SpecAssertion`] as the file lists it: `ref` in place of an id that is
/// its prefix and `subject_id`, and indexed evidence.
#[derive(Serialize, Deserialize)]
struct FileAssertion<'a> {
    #[serde(default, skip_serializing_if = "Option::is_none", borrow)]
    id: Option<std::borrow::Cow<'a, str>>,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    derived: Option<Ref>,
    #[serde(borrow)]
    subject_id: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    subject_kind: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    summary: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    provenance: FileProvenance<'a>,
    #[serde(default, skip_serializing_if = "Option::is_none", borrow)]
    edge_label: Option<std::borrow::Cow<'a, str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    omission: Option<Omission>,
}

/// One artifact's line: its identity and its assertions by index.
#[derive(Serialize, Deserialize)]
struct FileArtifact<'a> {
    #[serde(borrow)]
    id: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    file_name: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    title: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    format: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    schema: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    content_id: std::borrow::Cow<'a, str>,
    assertions: Vec<u32>,
}

/// Why a bundle file cannot be written or read.
#[derive(Debug)]
pub enum BundleFileError {
    /// The underlying reader or writer failed.
    Io(io::Error),
    /// The file is not JSON of either version.
    Parse(serde_json::Error),
    /// The file names a `schema` this build does not read.
    UnknownSchema(String),
    /// Two artifacts share a file name, so `files` cannot hold both.
    RepeatedFileName(String),
    /// An index, or a file name, points at nothing in the file.
    Dangling(String),
}

impl std::fmt::Display for BundleFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "bundle file I/O failed: {err}"),
            Self::Parse(err) => write!(f, "bundle file is not valid: {err}"),
            Self::UnknownSchema(schema) => write!(
                f,
                "bundle file schema {schema:?} is not read by this version"
            ),
            Self::RepeatedFileName(name) => write!(f, "two artifacts share the file name {name:?}"),
            Self::Dangling(what) => write!(f, "bundle file refers to a missing {what}"),
        }
    }
}

impl std::error::Error for BundleFileError {}

impl From<io::Error> for BundleFileError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for BundleFileError {
    fn from(err: serde_json::Error) -> Self {
        Self::Parse(err)
    }
}

/// Writes a v2 bundle file one artifact at a time. Each [`push`] writes the
/// artifact's line to `artifacts` and each assertion not yet listed to
/// `assertions`; the content is the caller's to keep for `files`. Only the
/// source table, a digest per listed assertion and the file names are held
/// across artifacts.
///
/// [`push`]: BundleFileV2::push
#[derive(Default)]
pub struct BundleFileV2 {
    sources: Vec<(String, String)>,
    source_ids: HashMap<(String, String), u32>,
    listed: HashMap<[u8; 32], u32>,
    names: HashSet<String>,
    artifacts: usize,
    line: Vec<u8>,
}

impl BundleFileV2 {
    /// A writer with nothing written.
    pub fn new() -> Self {
        Self::default()
    }

    /// Write `artifact`'s line to `artifacts` and its new assertions to
    /// `assertions`. A file name an earlier artifact used is refused.
    pub fn push(
        &mut self,
        artifact: &SpecArtifact,
        artifacts: &mut dyn Write,
        assertions: &mut dyn Write,
    ) -> Result<(), BundleFileError> {
        if !self.names.insert(artifact.file_name.clone()) {
            return Err(BundleFileError::RepeatedFileName(
                artifact.file_name.clone(),
            ));
        }
        let mut indexes = Vec::with_capacity(artifact.assertions.len());
        for assertion in &artifact.assertions {
            indexes.push(self.list(assertion, assertions)?);
        }
        let line = FileArtifact {
            id: artifact.id.as_str().into(),
            file_name: artifact.file_name.as_str().into(),
            title: artifact.title.as_str().into(),
            format: artifact.format.as_str().into(),
            schema: artifact.schema.as_str().into(),
            content_id: artifact.content_id.as_str().into(),
            assertions: indexes,
        };
        artifacts.write_all(if self.artifacts == 0 { b"" } else { b",\n" })?;
        serde_json::to_writer(&mut *artifacts, &line)?;
        self.artifacts += 1;
        Ok(())
    }

    /// The index of `assertion` in the file's table, listing it first if
    /// no earlier artifact did.
    fn list(
        &mut self,
        assertion: &SpecAssertion,
        out: &mut dyn Write,
    ) -> Result<u32, BundleFileError> {
        let evidence = assertion
            .provenance
            .evidence
            .iter()
            .map(|row| Evidence {
                source: self.source(row),
                path: row.path.as_str().into(),
                byte_start: row.byte_start,
                byte_end: row.byte_end,
            })
            .collect();
        let derived = [Ref::Node, Ref::Edge].into_iter().find(|derived| {
            assertion
                .id
                .strip_prefix(derived.prefix())
                .is_some_and(|rest| rest == assertion.subject_id)
        });
        let provenance = &assertion.provenance;
        let line = FileAssertion {
            id: derived.is_none().then(|| assertion.id.as_str().into()),
            derived,
            subject_id: assertion.subject_id.as_str().into(),
            subject_kind: assertion.subject_kind.as_str().into(),
            summary: assertion.summary.as_str().into(),
            provenance: FileProvenance {
                tier: provenance.tier,
                confidence_tier: provenance.confidence_tier,
                evidence,
                extractor_id: provenance.extractor_id.as_str().into(),
                content_hash: provenance.content_hash.as_str().into(),
                method: provenance.method.clone(),
                basis: provenance.basis.clone(),
            },
            edge_label: assertion.edge_label.as_deref().map(Into::into),
            omission: assertion.omission.clone(),
        };
        self.line.clear();
        serde_json::to_writer(&mut self.line, &line)?;
        let digest = *blake3::hash(&self.line).as_bytes();
        let next = u32::try_from(self.listed.len()).expect("fewer than 2^32 assertions");
        if let Some(index) = self.listed.get(&digest) {
            return Ok(*index);
        }
        out.write_all(if next == 0 { b"" } else { b",\n" })?;
        out.write_all(&self.line)?;
        self.listed.insert(digest, next);
        Ok(next)
    }

    fn source(&mut self, row: &EvidenceRef) -> u32 {
        let key = (row.repo.clone(), row.commit_sha.clone());
        if let Some(index) = self.source_ids.get(&key) {
            return *index;
        }
        let index = u32::try_from(self.sources.len()).expect("fewer than 2^32 sources");
        self.sources.push(key.clone());
        self.source_ids.insert(key, index);
        index
    }

    /// Write the complete file to `out`: the bundle's own fields from
    /// `head`, the source table, the artifact lines and assertion lines
    /// pushed so far (read back from `artifacts` and `assertions`), then
    /// `files`, whose entries `files` writes with [`BundleFileV2::file_entry`].
    pub fn finish<W: Write>(
        self,
        head: &SpecBundle<()>,
        out: &mut W,
        artifacts: &mut dyn Read,
        assertions: &mut dyn Read,
        files: impl FnOnce(&mut W) -> io::Result<()>,
    ) -> Result<(), BundleFileError> {
        out.write_all(b"{\"schema\":")?;
        serde_json::to_writer(&mut *out, BUNDLE_FILE_SCHEMA_V2)?;
        out.write_all(b",\n\"mode\":")?;
        serde_json::to_writer(&mut *out, &head.mode)?;
        for (key, count) in [
            ("assertion_count", head.assertion_count),
            ("gap_count", head.gap_count),
            ("drift_count", head.drift_count),
            ("security_count", head.security_count),
        ] {
            write!(out, ",\n\"{key}\":{count}")?;
        }
        if let Some(basis) = &head.graph_basis {
            out.write_all(b",\n\"graph_basis\":")?;
            serde_json::to_writer(&mut *out, basis)?;
        }
        if let Some(basis) = &head.curated_basis {
            out.write_all(b",\n\"curated_basis\":")?;
            serde_json::to_writer(&mut *out, basis)?;
        }
        out.write_all(b",\n\"sources\":[")?;
        for (i, (repo, commit_sha)) in self.sources.iter().enumerate() {
            out.write_all(if i == 0 { b"\n" } else { b",\n" })?;
            serde_json::to_writer(
                &mut *out,
                &Source {
                    repo: repo.as_str().into(),
                    commit_sha: commit_sha.as_str().into(),
                },
            )?;
        }
        out.write_all(if self.sources.is_empty() {
            b"]"
        } else {
            b"\n]"
        })?;
        let sections: [(&str, bool, &mut dyn Read); 2] = [
            ("artifacts", self.artifacts == 0, artifacts),
            ("assertions", self.listed.is_empty(), assertions),
        ];
        for (key, empty, lines) in sections {
            write!(out, ",\n\"{key}\":[")?;
            if !empty {
                out.write_all(b"\n")?;
                io::copy(lines, out)?;
                out.write_all(b"\n")?;
            }
            out.write_all(b"]")?;
        }
        out.write_all(b",\n\"files\":{")?;
        files(out)?;
        out.write_all(if self.artifacts == 0 {
            b"}}\n"
        } else {
            b"\n}}\n"
        })?;
        Ok(())
    }

    /// One `files` entry: the `index`th artifact's file name and its content,
    /// already encoded as a JSON string.
    pub fn file_entry(
        out: &mut dyn Write,
        index: usize,
        name: &str,
        content_json: &[u8],
    ) -> io::Result<()> {
        out.write_all(if index == 0 { b"\n" } else { b",\n" })?;
        serde_json::to_writer(&mut *out, name)?;
        out.write_all(b":")?;
        out.write_all(content_json)
    }
}

/// The v2 bundle file of `bundle`, written to `out`: [`BundleFileV2`] over
/// in-memory buffers, for a bundle already held whole.
pub fn write_bundle_file(bundle: &SpecBundle, out: &mut impl Write) -> Result<(), BundleFileError> {
    let mut writer = BundleFileV2::new();
    let (mut artifacts, mut assertions) = (Vec::new(), Vec::new());
    for artifact in &bundle.artifacts {
        writer.push(artifact, &mut artifacts, &mut assertions)?;
    }
    let head = SpecBundle {
        mode: bundle.mode,
        artifacts: Vec::new(),
        assertion_count: bundle.assertion_count,
        gap_count: bundle.gap_count,
        drift_count: bundle.drift_count,
        security_count: bundle.security_count,
        graph_basis: bundle.graph_basis.clone(),
        curated_basis: bundle.curated_basis.clone(),
    };
    writer.finish(
        &head,
        out,
        &mut artifacts.as_slice(),
        &mut assertions.as_slice(),
        |out| {
            for (i, artifact) in bundle.artifacts.iter().enumerate() {
                let content = serde_json::to_vec(&artifact.content)?;
                BundleFileV2::file_entry(out, i, &artifact.file_name, &content)?;
            }
            Ok(())
        },
    )
}

/// A v2 file as it parses.
#[derive(Deserialize)]
struct FileV2<'a> {
    mode: ExportMode,
    assertion_count: usize,
    gap_count: usize,
    drift_count: usize,
    security_count: usize,
    #[serde(default)]
    graph_basis: Option<GraphBasis>,
    #[serde(default)]
    curated_basis: Option<CuratedBasis>,
    #[serde(borrow)]
    sources: Vec<Source<'a>>,
    #[serde(borrow)]
    artifacts: Vec<FileArtifact<'a>>,
    #[serde(borrow)]
    assertions: Vec<FileAssertion<'a>>,
    files: HashMap<String, String>,
}

/// Only the `schema`, to tell the versions apart.
#[derive(Deserialize)]
struct Version {
    #[serde(default)]
    schema: Option<String>,
}

/// Read a bundle file of either version: every artifact with its content
/// and its full assertions. A v1 file is the AC-0740 shape, read as it was
/// written; a v2 file is expanded from its tables (ADR-0086).
pub fn read_bundle_file(text: &str) -> Result<SpecBundle, BundleFileError> {
    match serde_json::from_str::<Version>(text)?.schema.as_deref() {
        None => Ok(serde_json::from_str(text)?),
        Some(BUNDLE_FILE_SCHEMA_V2) => expand(serde_json::from_str(text)?),
        Some(other) => Err(BundleFileError::UnknownSchema(other.to_string())),
    }
}

fn expand(file: FileV2<'_>) -> Result<SpecBundle, BundleFileError> {
    let FileV2 {
        mode,
        assertion_count,
        gap_count,
        drift_count,
        security_count,
        graph_basis,
        curated_basis,
        sources,
        artifacts,
        assertions,
        mut files,
    } = file;
    let assertions = assertions
        .into_iter()
        .map(|line| assertion(line, &sources))
        .collect::<Result<Vec<_>, _>>()?;
    let artifacts = artifacts
        .into_iter()
        .map(|line| {
            let content = files
                .remove(line.file_name.as_ref())
                .ok_or_else(|| BundleFileError::Dangling(format!("file {:?}", line.file_name)))?;
            let listed = line
                .assertions
                .iter()
                .map(|&i| {
                    assertions
                        .get(i as usize)
                        .cloned()
                        .ok_or_else(|| BundleFileError::Dangling(format!("assertion {i}")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(SpecArtifact {
                id: line.id.into_owned(),
                file_name: line.file_name.into_owned(),
                title: line.title.into_owned(),
                format: line.format.into_owned(),
                schema: line.schema.into_owned(),
                content_id: line.content_id.into_owned(),
                content,
                assertions: listed,
            })
        })
        .collect::<Result<Vec<_>, BundleFileError>>()?;
    Ok(SpecBundle {
        mode,
        artifacts,
        assertion_count,
        gap_count,
        drift_count,
        security_count,
        graph_basis,
        curated_basis,
    })
}

fn assertion(
    line: FileAssertion<'_>,
    sources: &[Source<'_>],
) -> Result<SpecAssertion, BundleFileError> {
    let id = match (line.id, line.derived) {
        (Some(id), None) => id.into_owned(),
        (None, Some(derived)) => format!("{}{}", derived.prefix(), line.subject_id),
        _ => return Err(BundleFileError::Dangling("assertion id".into())),
    };
    let provenance = line.provenance;
    let evidence = provenance
        .evidence
        .into_iter()
        .map(|row| {
            let source = sources
                .get(row.source as usize)
                .ok_or_else(|| BundleFileError::Dangling(format!("source {}", row.source)))?;
            Ok(EvidenceRef {
                repo: source.repo.clone().into_owned(),
                path: row.path.into_owned(),
                byte_start: row.byte_start,
                byte_end: row.byte_end,
                commit_sha: source.commit_sha.clone().into_owned(),
            })
        })
        .collect::<Result<Vec<_>, BundleFileError>>()?;
    Ok(SpecAssertion {
        id,
        subject_id: line.subject_id.into_owned(),
        subject_kind: line.subject_kind.into_owned(),
        summary: line.summary.into_owned(),
        provenance: Provenance {
            tier: provenance.tier,
            confidence_tier: provenance.confidence_tier,
            evidence,
            extractor_id: provenance.extractor_id.into_owned(),
            content_hash: provenance.content_hash.into_owned(),
            method: provenance.method,
            basis: provenance.basis,
        },
        edge_label: line.edge_label.map(std::borrow::Cow::into_owned),
        omission: line.omission,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ui/src/fixtures/spec-bundle-file.input.json`: a Workbench preview and
    /// the full artifacts it stands for.
    #[derive(Deserialize)]
    struct Fixture {
        preview: SpecBundle<serde_json::Value>,
        full_artifacts: Vec<SpecArtifact>,
    }

    const INPUT: &str = include_str!("../../../ui/src/fixtures/spec-bundle-file.input.json");
    /// The v1 file of `INPUT` (AC-0740).
    const V1: &str = include_str!("../../../ui/src/fixtures/spec-bundle-file.v1.json");
    /// The v2 file of `INPUT` (ADR-0086).
    const V2: &str = include_str!("../../../ui/src/fixtures/spec-bundle-file.expected.json");

    fn fixture_bundle() -> SpecBundle {
        let fixture: Fixture = serde_json::from_str(INPUT).expect("fixture parses");
        let preview = fixture.preview;
        SpecBundle {
            mode: preview.mode,
            artifacts: fixture.full_artifacts,
            assertion_count: preview.assertion_count,
            gap_count: preview.gap_count,
            drift_count: preview.drift_count,
            security_count: preview.security_count,
            graph_basis: preview.graph_basis,
            curated_basis: preview.curated_basis,
        }
    }

    fn v2(bundle: &SpecBundle) -> String {
        let mut out = Vec::new();
        write_bundle_file(bundle, &mut out).expect("writes");
        String::from_utf8(out).unwrap()
    }

    fn assertion(id: &str, subject_id: &str, repo: &str, path: &str) -> SpecAssertion {
        SpecAssertion {
            id: id.into(),
            subject_id: subject_id.into(),
            subject_kind: "Endpoint".into(),
            summary: format!("summary of {id}"),
            provenance: Provenance {
                tier: Tier::Deterministic,
                confidence_tier: ConfidenceTier::Confirmed,
                evidence: vec![EvidenceRef {
                    repo: repo.into(),
                    path: path.into(),
                    byte_start: 1,
                    byte_end: 9,
                    commit_sha: "abc123".into(),
                }],
                extractor_id: "spec.bundle-file.test".into(),
                content_hash: format!("b3:{id}"),
                method: ProducingMethod::default(),
                basis: None,
            },
            edge_label: None,
            omission: None,
        }
    }

    fn artifact(id: &str, file_name: &str, assertions: Vec<SpecAssertion>) -> SpecArtifact {
        SpecArtifact {
            id: id.into(),
            file_name: file_name.into(),
            title: id.into(),
            format: "markdown".into(),
            schema: SpecArtifact::expected_schema(id),
            content_id: format!("b3:{id}"),
            content: format!("# {id}\n\n\"quoted\" and \\ and é\n"),
            assertions,
        }
    }

    fn bundle(artifacts: Vec<SpecArtifact>) -> SpecBundle {
        SpecBundle {
            mode: ExportMode::VerifiedOnly,
            assertion_count: artifacts.iter().map(|a| a.assertions.len()).sum(),
            artifacts,
            gap_count: 0,
            drift_count: 0,
            security_count: 0,
            graph_basis: None,
            curated_basis: None,
        }
    }

    // ADR-0086 (#1582): the v2 file of the AC-0740 fixture is pinned byte for
    // byte, reads back to the bundle it was written from, and the v1 file of
    // the same fixture reads to that same bundle.
    #[test]
    fn the_fixture_writes_the_pinned_v2_file_and_both_versions_read_alike() {
        let bundle = fixture_bundle();
        let written = v2(&bundle);
        assert_eq!(written, V2);
        assert_eq!(read_bundle_file(&written).unwrap(), bundle);
        assert_eq!(read_bundle_file(V1).unwrap(), bundle);
    }

    // ADR-0086 decisions 2 to 6: content only in `files`, one source row per
    // repo and commit, `ref` for a derivable id and the id kept otherwise,
    // a shared assertion listed once and referenced by index, one entry per
    // line; and the file reads back to the bundle.
    #[test]
    fn each_piece_is_written_once() {
        let shared = assertion(
            "node:endpoint:POST /orders",
            "endpoint:POST /orders",
            "acme/shop",
            "src/a.ts",
        );
        let edge = assertion(
            "edge:svc WRITES table",
            "svc WRITES table",
            "acme/shop",
            "src/b.ts",
        );
        let hop = assertion("flow:t:0:CALLS:x", "a CALLS x", "acme/lib", "lib.ts");
        let bundle = bundle(vec![
            artifact("readme", "README.md", vec![shared.clone(), edge.clone()]),
            artifact(
                "flow-dossiers",
                "flow-dossiers.md",
                vec![hop.clone(), shared.clone(), shared.clone()],
            ),
        ]);
        let written = v2(&bundle);
        let value: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(value["schema"], BUNDLE_FILE_SCHEMA_V2);
        assert_eq!(
            value["sources"],
            serde_json::json!([
                {"repo": "acme/shop", "commit_sha": "abc123"},
                {"repo": "acme/lib", "commit_sha": "abc123"},
            ])
        );
        assert_eq!(
            value["artifacts"][0]["assertions"],
            serde_json::json!([0, 1])
        );
        assert_eq!(
            value["artifacts"][1]["assertions"],
            serde_json::json!([2, 0, 0])
        );
        assert!(value["artifacts"][0].get("content").is_none());
        let table = value["assertions"].as_array().unwrap();
        assert_eq!(table.len(), 3);
        assert_eq!(
            (table[0]["ref"].as_str(), table[0].get("id")),
            (Some("node"), None)
        );
        assert_eq!(table[1]["ref"], "edge");
        assert_eq!(
            (table[2]["id"].as_str(), table[2].get("ref")),
            (Some("flow:t:0:CALLS:x"), None)
        );
        assert_eq!(table[2]["provenance"]["evidence"][0]["source"], 1);
        assert!(table[0]["provenance"]["evidence"][0].get("repo").is_none());
        assert_eq!(value["files"]["README.md"], bundle.artifacts[0].content);
        assert_eq!(written.matches("summary of node:endpoint").count(), 1);
        assert_eq!(written.matches("\"quoted\"").count(), 0);
        assert_eq!(written.matches("\\\"quoted\\\"").count(), 2);
        // One entry per line: every source, artifact and assertion opens a line.
        for line in [
            "{\"repo\":\"acme/shop\"",
            "{\"id\":\"readme\"",
            "{\"id\":\"flow-dossiers\"",
            "{\"ref\":\"node\"",
            "{\"ref\":\"edge\"",
            "{\"id\":\"flow:t:0:CALLS:x\"",
            "\"README.md\":",
        ] {
            assert!(
                written.lines().any(|l| l.starts_with(line)),
                "{line} opens a line"
            );
        }
        assert_eq!(read_bundle_file(&written).unwrap(), bundle);

        let empty = self::bundle(Vec::new());
        assert_eq!(read_bundle_file(&v2(&empty)).unwrap(), empty);
    }

    // ADR-0086 decision 11: the recorded budget. A register-shaped bundle,
    // its assertions repeated by a second artifact as the gap register and
    // rule evidence repeat theirs, is under 45% of its v1 file.
    #[test]
    fn a_register_shaped_bundle_is_under_the_recorded_budget() {
        let rows: Vec<_> = (0..500)
            .map(|i| {
                assertion(
                    &format!("node:gap:{i}"),
                    &format!("gap:{i}"),
                    "local/0f3a9b2c-registry-key",
                    &format!("src/module{}/file{i}.ts", i % 17),
                )
            })
            .collect();
        let bundle = bundle(vec![
            artifact("gap-register", "gap_register.md", rows.clone()),
            artifact("rule-evidence", "rule-evidence.md", rows[..300].to_vec()),
        ]);
        let files: serde_json::Map<_, _> = bundle
            .artifacts
            .iter()
            .map(|a| (a.file_name.clone(), a.content.clone().into()))
            .collect();
        let mut v1 = serde_json::to_value(&bundle).unwrap();
        v1["files"] = files.into();
        let v1 = serde_json::to_string_pretty(&v1).unwrap().len();
        let v2 = v2(&bundle).len();
        assert!(v2 * 100 < v1 * 45, "v2 {v2} bytes against v1 {v1}");
    }

    #[test]
    fn a_repeated_file_name_an_unknown_schema_and_a_dangling_index_are_refused() {
        let twice = bundle(vec![
            artifact("readme", "README.md", Vec::new()),
            artifact("other", "README.md", Vec::new()),
        ]);
        assert!(matches!(
            write_bundle_file(&twice, &mut Vec::new()),
            Err(BundleFileError::RepeatedFileName(name)) if name == "README.md"
        ));
        assert!(matches!(
            read_bundle_file("{\"schema\":\"cartograph.spec-bundle-file/v9\"}"),
            Err(BundleFileError::UnknownSchema(_))
        ));
        let one = bundle(vec![artifact(
            "readme",
            "README.md",
            vec![assertion("node:a", "a", "acme/shop", "a.ts")],
        )]);
        let written = v2(&one);
        for (from, to) in [
            ("\"assertions\":[0]", "\"assertions\":[1]"),
            ("\"source\":0", "\"source\":3"),
            ("\"README.md\":", "\"OTHER.md\":"),
        ] {
            let broken = written.replacen(from, to, 1);
            assert_ne!(broken, written, "{from} is in the file");
            assert!(
                matches!(read_bundle_file(&broken), Err(BundleFileError::Dangling(_))),
                "{to}"
            );
        }
    }
}
