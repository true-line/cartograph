//! Bounded verification of staged source against the separately retained Git-blob proof.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Component, Path, PathBuf},
};

const MAX_DOCUMENT: usize = 64 * 1024;
const MAX_FILES: usize = 64;
const MAX_FILE: usize = 2 * 1024 * 1024;
const MAX_TOTAL: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct VerifiedInput {
    pub repo: String,
    pub commit_sha: String,
    pub manifest_sha256: String,
    pub proof_sha256: String,
    pub files: BTreeMap<String, VerifiedFile>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct VerifiedFile {
    pub byte_len: u64,
    pub sha256: String,
    pub blake3: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    repo: String,
    commit_sha: String,
    scope: String,
    source_files: Vec<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    commit_sha: String,
    files_sha256: BTreeMap<String, String>,
    manifest_sha256: String,
    repo: String,
}
fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}
fn hash(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn relative(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && !value.contains(['\\', ':'])
        && value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && Path::new(value)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}
pub(super) fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "Input metadata unavailable.")?;
    if !metadata.file_type().is_file() || metadata.len() > maximum as u64 {
        return Err("Input must be a bounded regular file, never a symlink.".into());
    }
    let file = std::fs::File::open(path).map_err(|_| "Input file unavailable.")?;
    if !file
        .metadata()
        .map_err(|_| "Input handle unavailable.")?
        .is_file()
    {
        return Err("Input handle is not a regular file.".into());
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Input read failed.")?;
    if bytes.len() > maximum || bytes.len() as u64 != metadata.len() {
        return Err("Input changed or exceeded its bound.".into());
    }
    Ok(bytes)
}
fn document<T: serde::de::DeserializeOwned + Serialize>(bytes: &[u8]) -> Result<T, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Input document is not UTF-8.")?;
    agents::investigation::decode_record(text)
        .map_err(|_| "Input document violates its strict schema.".into())
}
// Count every entry (including empty directories), so an unexpected tree cannot
// force an unbounded walk. No symlink is followed. Production capture independently
// validates handles and binds the bytes consumed after this preparation check.
fn membership(root: &Path) -> Result<BTreeSet<String>, String> {
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut files = BTreeSet::new();
    let mut entries = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        if depth > 32 {
            return Err("Source tree exceeds depth limit.".into());
        }
        for entry in std::fs::read_dir(directory).map_err(|_| "Source directory unavailable.")? {
            entries += 1;
            if entries > 512 {
                return Err("Source tree exceeds entry limit.".into());
            }
            let entry = entry.map_err(|_| "Source entry unavailable.")?;
            let kind = entry
                .file_type()
                .map_err(|_| "Source entry type unavailable.")?;
            if kind.is_dir() {
                pending.push((entry.path(), depth + 1));
            } else if kind.is_file() {
                let path = entry.path();
                // Manifest keys use '/' on every platform; normalize filesystem
                // components without relaxing validation of manifest strings.
                let name = path
                    .strip_prefix(root)
                    .ok()
                    .and_then(|relative| {
                        relative
                            .components()
                            .map(|component| match component {
                                Component::Normal(name) => name.to_str(),
                                _ => None,
                            })
                            .collect::<Option<Vec<_>>>()
                    })
                    .ok_or("Source path is not a valid relative UTF-8 path.")?
                    .join("/");
                if !relative(&name) || files.len() >= MAX_FILES {
                    return Err("Source path/count invalid.".into());
                }
                files.insert(name);
            } else {
                return Err("Source contains a symlink or nonregular entry.".into());
            }
        }
    }
    Ok(files)
}

pub(super) fn verify(root: &Path, manifest: &Path, proof: &Path) -> Result<VerifiedInput, String> {
    if !root.is_absolute()
        || !std::fs::symlink_metadata(root)
            .map_err(|_| "Source root unavailable.")?
            .file_type()
            .is_dir()
    {
        return Err("Source root must be an absolute real directory.".into());
    }
    let root = root
        .canonicalize()
        .map_err(|_| "Source root cannot be resolved.")?;
    let external = |path: &Path| -> Result<PathBuf, String> {
        if !path.is_absolute()
            || std::fs::symlink_metadata(path)
                .map_err(|_| "Proof metadata unavailable.")?
                .file_type()
                .is_symlink()
        {
            return Err("Proof documents must be absolute nonsymlink files.".into());
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| "Proof document unavailable.")?;
        if canonical.starts_with(&root) {
            return Err("Proof/evaluator documents cannot be staged source.".into());
        }
        Ok(canonical)
    };
    let manifest_bytes = read_bounded(&external(manifest)?, MAX_DOCUMENT)?;
    let proof_bytes = read_bounded(&external(proof)?, MAX_DOCUMENT)?;
    let manifest: Manifest = document(&manifest_bytes)?;
    let proof: Proof = document(&proof_bytes)?;
    let selected: BTreeSet<_> = manifest.source_files.iter().cloned().collect();
    if manifest.schema_version != 1
        || manifest.repo.is_empty()
        || manifest.repo.len() > 256
        || !hash(&manifest.commit_sha, 40)
        || manifest.scope.is_empty()
        || selected.is_empty()
        || selected.len() > MAX_FILES
        || selected.len() != manifest.source_files.len()
        || selected.iter().any(|path| !relative(path))
        || proof.repo != manifest.repo
        || proof.commit_sha != manifest.commit_sha
        || proof.manifest_sha256 != sha256(&manifest_bytes)
        || proof.files_sha256.keys().cloned().collect::<BTreeSet<_>>() != selected
        || proof.files_sha256.values().any(|value| !hash(value, 64))
        || membership(&root)? != selected
    {
        return Err("Source manifest, proof, or exact membership differs.".into());
    }
    let mut files = BTreeMap::new();
    let mut total = 0u64;
    for path in selected {
        let bytes = read_bounded(&root.join(&path), MAX_FILE)?;
        total += bytes.len() as u64;
        let actual = sha256(&bytes);
        if total > MAX_TOTAL || proof.files_sha256[&path] != actual {
            return Err("Source bytes differ from proof or exceed total limit.".into());
        }
        files.insert(
            path,
            VerifiedFile {
                byte_len: bytes.len() as u64,
                sha256: actual,
                blake3: core_prov::content_hash(&bytes),
            },
        );
    }
    if membership(&root)? != files.keys().cloned().collect() {
        return Err("Source membership changed while verifying.".into());
    }
    Ok(VerifiedInput {
        repo: manifest.repo,
        commit_sha: manifest.commit_sha,
        manifest_sha256: sha256(&manifest_bytes),
        proof_sha256: sha256(&proof_bytes),
        files,
    })
}

#[path = "input_tests.rs"]
mod tests;
