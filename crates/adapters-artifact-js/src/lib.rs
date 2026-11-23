//! JavaScript bundle artifact reader (ADR-0051 §5 slice 8, #1445).
//!
//! A bundle that names its source map (`//# sourceMappingURL=`, a sibling
//! file or an inline `data:` URL) carries the original sources it was built
//! from when the map has `sourcesContent`. This reader recovers those
//! sources and reads them with the TypeScript adapter, so a checkout that
//! ships only its built front end still gets files, symbols, calls, routes
//! and network calls. Each recovered source is placed under its bundle
//! (`public/app.js!/src/cart.ts`) and every fact read from it is
//! re-attributed: T0, extractor `t0.artifact-js`, with an `artifact` evidence
//! basis citing the bytes the source was carried in (the map file's digest,
//! kind `js-source-map`, or the bundle's for an inline map, kind
//! `js-bundle`) and the source's path as the member. Nothing is executed.
//! Business rules, state machines, infrastructure resources and what governs
//! them are never confirmed from an artifact alone (ADR-0051), so those
//! facts are held at `InferredStrong`. A bundle whose map cannot be read is
//! one Gap naming why.

pub mod source_map;

use core_graph::{Edge, Node};
use core_prov::{ArtifactLocator, ArtifactRef, ConfidenceTier, EvidenceBasis, Provenance, Tier};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Provenance extractor id of every fact this reader emits.
pub const EXTRACTOR_ID: &str = "t0.artifact-js";

/// Artifact kind of a bundle file (cited for an inline map, and by Gaps).
pub const BUNDLE_KIND: &str = "js-bundle";

/// Artifact kind of a source map file.
pub const MAP_KIND: &str = "js-source-map";

/// Largest bundle or map read; a larger one that names a map is a Gap.
pub const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

/// Bytes read from the end of a too-large file to tell whether it names a
/// map.
const TAIL_BYTES: u64 = 4096;

/// Labels never confirmed from an artifact alone (ADR-0051 decision 5).
const NOT_CONFIRMED: &[&str] = &["BusinessRule", "StateMachine", "Resource", "GOVERNS"];

/// Identity of the source being recovered.
pub struct SourceId<'a> {
    /// Repository identity.
    pub repo: &'a str,
    /// Commit SHA, or `workdir` for an unversioned tree.
    pub commit: &'a str,
}

/// Graph facts read from a set of bundles.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Extraction {
    /// Recovered nodes.
    pub nodes: Vec<Node>,
    /// Recovered edges.
    pub edges: Vec<Edge>,
}

/// `sha256:` and the lowercase hex digest of `bytes`.
pub fn digest(bytes: &[u8]) -> String {
    hex_digest(&Sha256::digest(bytes))
}

fn hex_digest(hash: &[u8]) -> String {
    let mut out = String::with_capacity(71);
    out.push_str("sha256:");
    for byte in hash {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// True for a bundle file name this reader considers.
fn is_bundle(rel: &str) -> bool {
    [".js", ".mjs", ".cjs"].iter().any(|ext| rel.ends_with(ext))
}

/// True for a recovered source the TypeScript adapter reads.
fn is_readable_source(path: &str) -> bool {
    adapters_lang_ts::SOURCE_EXTENSIONS
        .iter()
        .chain(&adapters_lang_ts::vue::COMPONENT_EXTENSIONS)
        .any(|ext| path.ends_with(ext))
        && !path.ends_with(".d.ts")
}

/// The bundler's own runtime modules (`webpack/runtime/…`,
/// `(webpack)/buildin/…`), not the application's source.
fn is_bundler_runtime(path: &str) -> bool {
    matches!(path.split('/').next(), Some("webpack" | "(webpack)"))
}

fn skipped_dir(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "target" | "build" | "out" | "node_modules" | "dist" | "generated"
        )
}

/// `/`-joined segments with `.` and empty segments dropped and `..`
/// popping one; a `..` above the top is refused when `strict`, dropped
/// otherwise.
fn normalize<'a>(segments: impl Iterator<Item = &'a str>, strict: bool) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for segment in segments {
        match segment {
            "" | "." => {}
            ".." => {
                if out.pop().is_none() && strict {
                    return None;
                }
            }
            segment => out.push(segment),
        }
    }
    (!out.is_empty()).then(|| out.join("/"))
}

fn dir_of(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The repository-relative path a mapped source URL names: a scheme and its
/// authority (`webpack://app/`) are dropped and the rest read from the
/// root, a `/`-rooted URL is read from the root, and a relative URL is read
/// from the map's directory. A query or fragment is dropped. `None` when
/// nothing is left or the path is not a plain relative member path (no
/// backslash, NUL or colon, at most [`core_prov::MAX_MEMBER_PATH_BYTES`]).
fn source_path(url: &str, map_dir: &str) -> Option<String> {
    let url = url.split(['?', '#']).next().unwrap_or_default();
    let path = if let Some((_, rest)) = url.split_once("://") {
        let tail = rest.split_once('/').map_or("", |(_, tail)| tail);
        normalize(tail.split('/'), false)
    } else if let Some(rest) = url.strip_prefix('/') {
        normalize(rest.split('/'), false)
    } else {
        normalize(map_dir.split('/').chain(url.split('/')), false)
    }?;
    // A colon is a drive or a stream on some platform; the path is also the
    // member the evidence cites, so it keeps to that bound.
    (!path.contains(['\\', '\0', ':']) && path.len() <= core_prov::MAX_MEMBER_PATH_BYTES)
        .then_some(path)
}

/// A bundle as read: its bytes, or the digest and length of one larger than
/// [`MAX_ARTIFACT_BYTES`] whose tail names a map.
enum Content {
    Bytes(Vec<u8>),
    TooLarge {
        digest: String,
        len: u64,
    },
    /// A large file that names no map.
    Skipped,
}

fn read_bundle_file(path: &std::path::Path) -> std::io::Result<Content> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len <= MAX_ARTIFACT_BYTES as u64 {
        let mut bytes = Vec::with_capacity(len as usize);
        file.read_to_end(&mut bytes)?;
        return Ok(Content::Bytes(bytes));
    }
    file.seek(SeekFrom::Start(len - TAIL_BYTES))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    if source_map::mapping_url(&tail).is_none() {
        return Ok(Content::Skipped);
    }
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(Content::TooLarge {
        digest: hex_digest(&hasher.finalize()),
        len,
    })
}

fn artifact_basis(refs: Vec<ArtifactRef>) -> EvidenceBasis {
    EvidenceBasis::Artifact { refs }
}

fn gap(id: &SourceId<'_>, bundle: &str, code: &str, detail: &str, cited: &Cited) -> Node {
    let gap = format!("gap:{}@{bundle}#js-bundle", id.repo);
    let provenance = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Gap,
        Vec::new(),
        EXTRACTOR_ID,
        format!("Gap {gap}").as_bytes(),
    )
    .expect("Gap is within every ceiling")
    .with_basis(artifact_basis(vec![cited.whole()]));
    Node {
        id: gap,
        label: "Gap".into(),
        props: serde_json::json!({
            "path": bundle,
            "reason": format!("unreadable JavaScript source map: {detail}"),
            "reason_code": code,
            "attempted_tiers": ["T0"],
            "prov": provenance,
        }),
    }
}

/// The artifact a fact is cited from: its digest, kind and length.
#[derive(Clone)]
struct Cited {
    digest: String,
    kind: &'static str,
    len: u64,
}

impl Cited {
    fn of(bytes: &[u8], kind: &'static str) -> Cited {
        Cited {
            digest: digest(bytes),
            kind,
            len: bytes.len() as u64,
        }
    }

    fn whole(&self) -> ArtifactRef {
        ArtifactRef {
            digest: self.digest.clone(),
            kind: self.kind.into(),
            at: ArtifactLocator::Offset {
                start: 0,
                end: self.len.max(1),
            },
        }
    }

    fn member(&self, path: &str) -> ArtifactRef {
        ArtifactRef {
            digest: self.digest.clone(),
            kind: self.kind.into(),
            at: ArtifactLocator::Member { path: path.into() },
        }
    }
}

/// One original source recovered from a bundle's map.
struct Recovered {
    /// Path the source is read under: `{bundle}!/{member}`.
    path: String,
    /// Repository-relative path the map names.
    member: String,
    content: String,
    cited: Cited,
}

/// What reading one bundle gave: recovered sources and Gap nodes.
#[derive(Default)]
struct Read {
    recovered: Vec<Recovered>,
    gaps: Vec<Node>,
}

/// Read one bundle: find its map, parse it and keep each mapped source the
/// TypeScript adapter reads whose path `present` does not already hold.
fn read_bundle(
    id: &SourceId<'_>,
    bundle: &str,
    bytes: &[u8],
    present: &BTreeSet<String>,
    read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
    out: &mut Read,
) {
    let Some(url) = source_map::mapping_url(bytes) else {
        return;
    };
    let bundle_cited = Cited::of(bytes, BUNDLE_KIND);
    let (map, cited, map_dir) = match source_map::data_url(&url) {
        Some(Ok(map)) => (map, bundle_cited.clone(), dir_of(bundle).to_string()),
        Some(Err(())) => {
            out.gaps.push(gap(
                id,
                bundle,
                "bad_data_url",
                "the inline map does not decode",
                &bundle_cited,
            ));
            return;
        }
        None => {
            let plain = url.split(['?', '#']).next().unwrap_or_default();
            let map_rel =
                (!plain.contains("://") && !plain.starts_with('/') && !plain.contains('\\'))
                    .then(|| normalize(dir_of(bundle).split('/').chain(plain.split('/')), true))
                    .flatten();
            let Some(map_rel) = map_rel else {
                out.gaps.push(gap(
                    id,
                    bundle,
                    "map_outside",
                    "the map is named outside the repository",
                    &bundle_cited,
                ));
                return;
            };
            let map = match read(&map_rel) {
                Ok(map) => map,
                Err(_) => {
                    out.gaps.push(gap(
                        id,
                        bundle,
                        "map_missing",
                        &format!("{map_rel} cannot be read"),
                        &bundle_cited,
                    ));
                    return;
                }
            };
            if map.len() > MAX_ARTIFACT_BYTES {
                out.gaps.push(gap(
                    id,
                    bundle,
                    "too_large",
                    &format!("{map_rel} is over {MAX_ARTIFACT_BYTES} bytes"),
                    &Cited::of(&map, MAP_KIND),
                ));
                return;
            }
            let cited = Cited::of(&map, MAP_KIND);
            let dir = dir_of(&map_rel).to_string();
            (map, cited, dir)
        }
    };
    let sources = match source_map::sources(&map) {
        Ok(sources) => sources,
        Err(refusal) => {
            out.gaps.push(gap(
                id,
                bundle,
                refusal.code(),
                "the map is not a version 3 source map",
                &cited,
            ));
            return;
        }
    };
    let mut seen = BTreeSet::new();
    let mut without_content = 0usize;
    for source in sources {
        let Some(member) = source_path(&source.url, &map_dir) else {
            continue;
        };
        if member.split('/').any(|segment| segment == "node_modules")
            || is_bundler_runtime(&member)
            || !is_readable_source(&member)
            || present.contains(&member)
            || !seen.insert(member.clone())
        {
            continue;
        }
        let Some(content) = source.content else {
            without_content += 1;
            continue;
        };
        out.recovered.push(Recovered {
            path: format!("{bundle}!/{member}"),
            member,
            content,
            cited: cited.clone(),
        });
    }
    if without_content > 0 {
        let mut node = gap(
            id,
            bundle,
            "no_sources_content",
            &format!("{without_content} mapped sources carry no sourcesContent"),
            &cited,
        );
        node.id = format!("gap:{}@{bundle}#js-sources-content", id.repo);
        out.gaps.push(node);
    }
}

/// Read `bundles` — `(repository-relative path, bytes)` pairs — into graph
/// facts, `read` returning a map file a bundle names and `present` holding
/// the repository paths whose source is already read (a mapped source at
/// one of them is not recovered). Output is the same whatever order the
/// bundles come in.
pub fn extract(
    id: &SourceId<'_>,
    bundles: &[(String, Vec<u8>)],
    present: &BTreeSet<String>,
    read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
) -> std::io::Result<Extraction> {
    let mut ordered: Vec<&(String, Vec<u8>)> = bundles.iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0));
    let mut reads = Read::default();
    for (bundle, bytes) in ordered {
        read_bundle(id, bundle, bytes, present, read, &mut reads);
    }
    finish(id, reads)
}

/// Read the recovered sources with the TypeScript adapter and re-attribute
/// every fact to the artifact it was carried in.
fn finish(id: &SourceId<'_>, reads: Read) -> std::io::Result<Extraction> {
    let mut out = Extraction {
        nodes: reads.gaps,
        edges: Vec::new(),
    };
    if reads.recovered.is_empty() {
        return Ok(out);
    }
    let staging = tempfile::tempdir()?;
    let mut by_path: BTreeMap<&str, &Recovered> = BTreeMap::new();
    for source in &reads.recovered {
        let path = staging.path().join(&source.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &source.content)?;
        by_path.insert(source.path.as_str(), source);
    }
    let ts_id = adapters_lang_ts::SourceId {
        repo: id.repo,
        commit: id.commit,
    };
    let facts = adapters_lang_ts::extract_dir(staging.path(), &ts_id)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    for mut node in facts.nodes {
        let path = node.props.get("path").and_then(serde_json::Value::as_str);
        let owner = path.and_then(|path| by_path.get(path).copied());
        if restamp(&mut node.props, &node.label, &node.id, owner, &by_path) {
            out.nodes.push(node);
        }
    }
    for mut edge in facts.edges {
        if restamp(&mut edge.props, &edge.label, &edge.src, None, &by_path) {
            out.edges.push(edge);
        }
    }
    out.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    out.edges
        .sort_by(|a, b| (&a.src, &a.dst, &a.label).cmp(&(&b.src, &b.dst, &b.label)));
    Ok(out)
}

/// The recovered sources a fact rests on: those its evidence spans cite,
/// else the one its id names, else `owner`.
fn cited_sources<'a>(
    provenance: &Provenance,
    id: &str,
    owner: Option<&'a Recovered>,
    by_path: &BTreeMap<&str, &'a Recovered>,
) -> Vec<&'a Recovered> {
    let mut cited: BTreeMap<&str, &Recovered> = BTreeMap::new();
    for evidence in &provenance.evidence {
        if let Some(source) = by_path.get(evidence.path.as_str()) {
            cited.insert(source.path.as_str(), source);
        }
    }
    if cited.is_empty() {
        if let Some(source) = owner {
            cited.insert(source.path.as_str(), source);
        } else if let Some((_, source)) = by_path
            .iter()
            .rev()
            .find(|(path, _)| id.contains(&format!("@{path}")))
        {
            cited.insert(source.path.as_str(), source);
        }
    }
    cited.into_values().collect()
}

/// Re-attribute one fact's provenance to the artifacts its sources were
/// carried in. False when the fact cites no recovered source, so it cannot
/// be attributed and is left out.
fn restamp(
    props: &mut serde_json::Value,
    label: &str,
    id: &str,
    owner: Option<&Recovered>,
    by_path: &BTreeMap<&str, &Recovered>,
) -> bool {
    let Some(prov) = props.get("prov") else {
        return false;
    };
    let Ok(mut provenance) = serde_json::from_value::<Provenance>(prov.clone()) else {
        return false;
    };
    let sources = cited_sources(&provenance, id, owner, by_path);
    if sources.is_empty() {
        return false;
    }
    let mut refs: Vec<ArtifactRef> = sources
        .iter()
        .map(|source| source.cited.member(&source.member))
        .collect();
    refs.dedup();
    provenance.tier = Tier::Deterministic;
    provenance.evidence.clear();
    provenance.extractor_id = EXTRACTOR_ID.into();
    if NOT_CONFIRMED.contains(&label) && provenance.confidence_tier == ConfidenceTier::Confirmed {
        provenance.confidence_tier = ConfidenceTier::InferredStrong;
    }
    provenance.basis = Some(artifact_basis(refs));
    props["prov"] = serde_json::to_value(provenance).expect("provenance serializes");
    true
}

/// Read the bundles of the checkout at `root`: every `.js`, `.mjs` or
/// `.cjs` file the ordinary walk selects (`.gitignore` honored; hidden,
/// dependency and build-output directories not entered) that names a source
/// map. A mapped source whose path the walk already holds is read by the
/// source adapters, not recovered.
pub fn extract_dir(
    root: &std::path::Path,
    id: &SourceId<'_>,
    on_file: &mut dyn FnMut(&str),
) -> std::io::Result<Extraction> {
    let walked = walk(root)?;
    let present: BTreeSet<String> = walked.iter().cloned().collect();
    let read = |rel: &str| std::fs::read(root.join(rel));
    let mut reads = Read::default();
    for rel in walked.iter().filter(|rel| is_bundle(rel)) {
        match read_bundle_file(&root.join(rel))? {
            Content::Bytes(bytes) => {
                if source_map::mapping_url(&bytes).is_some() {
                    on_file(rel);
                }
                read_bundle(id, rel, &bytes, &present, &read, &mut reads);
            }
            Content::TooLarge { digest, len } => {
                on_file(rel);
                let cited = Cited {
                    digest,
                    kind: BUNDLE_KIND,
                    len,
                };
                reads.gaps.push(gap(
                    id,
                    rel,
                    "too_large",
                    &format!("the bundle is over {MAX_ARTIFACT_BYTES} bytes"),
                    &cited,
                ));
            }
            Content::Skipped => {}
        }
    }
    finish(id, reads)
}

/// [`extract_dir`] over a capture: `paths` are every path it retained and
/// `read` returns their retained bytes, not the checkout's. Bundles are the
/// retained `.js`, `.mjs` and `.cjs` paths outside the directories the
/// checkout walk does not enter, and which mapped sources are already read
/// is decided by the same capture.
pub fn extract_dir_from(
    id: &SourceId<'_>,
    paths: &[String],
    read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
    on_file: &mut dyn FnMut(&str),
) -> std::io::Result<Extraction> {
    // The checkout walk's directory skips apply to the capture too, so both
    // reads select the same bundles.
    let present: BTreeSet<String> = paths
        .iter()
        .filter(|rel| !dir_of(rel).split('/').any(skipped_dir))
        .cloned()
        .collect();
    let mut reads = Read::default();
    for rel in present.iter().filter(|rel| is_bundle(rel)) {
        let bytes = read(rel)?;
        if bytes.len() > MAX_ARTIFACT_BYTES {
            if source_map::mapping_url(&bytes[bytes.len() - TAIL_BYTES as usize..]).is_some() {
                on_file(rel);
                reads.gaps.push(gap(
                    id,
                    rel,
                    "too_large",
                    &format!("the bundle is over {MAX_ARTIFACT_BYTES} bytes"),
                    &Cited::of(&bytes, BUNDLE_KIND),
                ));
            }
            continue;
        }
        if source_map::mapping_url(&bytes).is_some() {
            on_file(rel);
        }
        read_bundle(id, rel, &bytes, &present, read, &mut reads);
    }
    finish(id, reads)
}

/// The bundles of the walk of `root` that name a map file, and the map
/// files they name, sorted: what a capture retains for [`extract_dir_from`]
/// beyond the source lanes.
pub fn artifact_paths(root: &std::path::Path) -> std::io::Result<Vec<String>> {
    let mut out = BTreeSet::new();
    for rel in walk(root)?.into_iter().filter(|rel| is_bundle(rel)) {
        let Ok(Content::Bytes(bytes)) = read_bundle_file(&root.join(&rel)) else {
            continue;
        };
        let Some(url) = source_map::mapping_url(&bytes) else {
            continue;
        };
        if url.starts_with("data:") || url.contains("://") || url.starts_with('/') {
            continue;
        }
        let plain = url.split(['?', '#']).next().unwrap_or_default();
        if let Some(map) = normalize(dir_of(&rel).split('/').chain(plain.split('/')), true)
            && root.join(&map).is_file()
        {
            out.insert(rel);
            out.insert(map);
        }
    }
    Ok(out.into_iter().collect())
}

/// Every file the ordinary walk of `root` selects, sorted.
fn walk(root: &std::path::Path) -> std::io::Result<Vec<String>> {
    let mut paths: Vec<String> =
        source_walk::files(root, &skipped_dir, source_walk::Gitignores::Honor)?
            .into_iter()
            .map(|file| file.rel)
            .collect();
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
mod tests;
