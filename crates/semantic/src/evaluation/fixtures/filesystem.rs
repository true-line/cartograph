use super::*;
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, File, OpenOptions};
use std::io::Read;

fn open(root: &Dir, path: &str) -> Result<File> {
    if !validation::path(path) {
        return Err(FixtureError::Invalid("confined relative path"));
    }
    let mut parts = path.split('/');
    let name = parts
        .next_back()
        .ok_or(FixtureError::Invalid("source name"))?;
    let mut dir = root.try_clone()?;
    for component in parts {
        dir = dir.open_dir_nofollow(component)?;
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let f = dir.open_with(name, &options)?;
    if !f.metadata()?.is_file() {
        return Err(FixtureError::Invalid("regular fixture file"));
    }
    Ok(f)
}
fn read(mut f: File, max: usize, expected: usize) -> Result<Vec<u8>> {
    if expected > max || f.metadata()?.len() != expected as u64 {
        return Err(FixtureError::Capacity);
    }
    let mut b = Vec::new();
    f.by_ref().take(expected as u64 + 1).read_to_end(&mut b)?;
    if b.len() > max || b.len() != expected {
        return Err(FixtureError::Capacity);
    }
    Ok(b)
}
pub(super) fn load(path: &Path, purpose: ExpectedPurpose) -> Result<LoadedFixtureBundle> {
    // The caller chooses the fixture root. Every descendant is opened relative
    // to this pinned directory, with no-follow at each component including files.
    let root = Dir::open_ambient_dir(path, cap_std::ambient_authority())?;
    let mut files = Vec::new();
    let mut total = 0usize;
    for name in ["inputs.json", "leakage.json", "labels.json"] {
        let f = open(&root, name)?;
        let len = usize::try_from(f.metadata()?.len()).map_err(|_| FixtureError::Capacity)?;
        if len > MAX_MANIFEST_BYTES {
            return Err(FixtureError::Capacity);
        }
        total = total.checked_add(len).ok_or(FixtureError::Capacity)?;
        files.push((f, len));
    }
    if total > MAX_BUNDLE_BYTES {
        return Err(FixtureError::Capacity);
    }
    let mut raw = Vec::new();
    for (f, len) in files {
        raw.push(read(f, MAX_MANIFEST_BYTES, len)?);
    }
    let inputs: InputsManifest = canonical::decode_for_purpose(&raw[0], "InputsManifest", purpose)?;
    let leakage: LeakageManifest =
        canonical::decode_for_purpose(&raw[1], "LeakageManifest", purpose)?;
    let labels: LabelsManifest = canonical::decode_for_purpose(&raw[2], "LabelsManifest", purpose)?;
    let digests = FixtureDigests {
        inputs: sha256(&canonical_json(&inputs)?),
        leakage: sha256(&canonical_json(&leakage)?),
        labels: sha256(&canonical_json(&labels)?),
    };
    validation::validate(&inputs, &leakage, &labels, &digests)?;
    // Open and check all retained source sizes, and admit complete bundle size,
    // before allocating/copying any retained source body. Handles pin each file.
    let mut pending = Vec::new();
    for s in &inputs.sources {
        let len = usize::try_from(s.byte_len).map_err(|_| FixtureError::Capacity)?;
        if len > MAX_SOURCE_BYTES {
            return Err(FixtureError::Capacity);
        }
        total = total.checked_add(len).ok_or(FixtureError::Capacity)?;
        if total > MAX_BUNDLE_BYTES {
            return Err(FixtureError::Capacity);
        }
        let f = open(&root, &s.bytes_path)?;
        if f.metadata()?.len() != s.byte_len {
            return Err(FixtureError::Invalid("retained source length"));
        }
        pending.push((s, f, len));
    }
    let mut sources = BTreeMap::new();
    for (s, f, len) in pending {
        let bytes = read(f, MAX_SOURCE_BYTES, len)?;
        if sha256(&bytes) != s.content_hash {
            return Err(FixtureError::Invalid("retained source digest"));
        }
        sources.insert(s.source_id.clone(), bytes);
    }
    Ok(LoadedFixtureBundle {
        inputs,
        leakage,
        labels,
        digests,
        sources,
    })
}
