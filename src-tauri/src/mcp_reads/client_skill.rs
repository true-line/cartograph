//! Read-only disclosure of the canonical, version-matched packaged client skill.
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, OpenOptions};
use serde::Serialize;
use std::{io::Read, path::Path};
use tauri::Manager;

const RESOURCE_SKILL: &str = "skills/read-cartograph-context";
const FILES: &[(&str, &[u8])] = &[
    (
        "SKILL.md",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.agents/skills/read-cartograph-context/SKILL.md"
        )),
    ),
    (
        "references/connection.md",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.agents/skills/read-cartograph-context/references/connection.md"
        )),
    ),
    (
        "references/measurement.md",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.agents/skills/read-cartograph-context/references/measurement.md"
        )),
    ),
    (
        "references/requests.md",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.agents/skills/read-cartograph-context/references/requests.md"
        )),
    ),
];

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum ClientSkill {
    Available {
        app_version: String,
        skill_path: String,
        files: Vec<SkillFile>,
    },
    Unavailable,
}
#[derive(Debug, Serialize)]
pub(crate) struct SkillFile {
    path: &'static str,
    content: String,
}

/// Returns packaged skill files only when they exactly match the embedded canonical bytes.
/// No activation, installation, configuration mutation or provider access.
#[tauri::command]
pub(crate) fn mcp_reads_client_skill(app: tauri::AppHandle) -> ClientSkill {
    app.path()
        .resource_dir()
        .ok()
        .and_then(|root| packaged_skill(&root, &app.package_info().version.to_string()))
        .unwrap_or(ClientSkill::Unavailable)
}

fn packaged_skill(resource_root: &Path, version: &str) -> Option<ClientSkill> {
    // Resource root is the platform-provided trust anchor. Each descendant is
    // opened relative to held directory handles without following symlinks.
    let root = Dir::open_ambient_dir(resource_root, cap_std::ambient_authority()).ok()?;
    let skills = root.open_dir_nofollow("skills").ok()?;
    let skill = skills.open_dir_nofollow("read-cartograph-context").ok()?;
    let references = skill.open_dir_nofollow("references").ok()?;
    let mut files = Vec::with_capacity(FILES.len());
    for &(path, expected) in FILES {
        let (directory, name) = match path.strip_prefix("references/") {
            Some(name) => (&references, name),
            None => (&skill, path),
        };
        let prior = directory.symlink_metadata(name).ok()?;
        if !prior.is_file() || prior.len() != u64::try_from(expected.len()).ok()? {
            return None;
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let mut file = directory.open_with(name, &options).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.len() != u64::try_from(expected.len()).ok()? {
            return None;
        }
        // The exact compile-time size is the allocation ceiling. Reading one
        // extra byte detects growth after metadata without an unbounded read.
        let limit = expected.len().checked_add(1)?;
        let mut bytes = Vec::with_capacity(limit);
        file.by_ref()
            .take(u64::try_from(limit).ok()?)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes != expected {
            return None;
        }
        files.push(SkillFile {
            path,
            content: String::from_utf8(bytes).ok()?,
        });
    }
    let location = resource_root.join(RESOURCE_SKILL).join("SKILL.md");
    let skill_path = location.to_str()?.to_owned();
    Some(ClientSkill::Available {
        app_version: version.to_owned(),
        skill_path,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for &(path, bytes) in FILES {
            let file = root.path().join(RESOURCE_SKILL).join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, bytes).unwrap();
        }
        root
    }
    #[test]
    fn packaged_skill_returns_exact_canonical_bytes_and_version() {
        // AC-0290: skill disclosure is independent of activation/provider state.
        let root = fixture();
        let ClientSkill::Available {
            app_version,
            skill_path,
            files,
        } = packaged_skill(root.path(), "fixture-version").unwrap()
        else {
            panic!("available")
        };
        assert_eq!(app_version, "fixture-version");
        assert_eq!(
            skill_path,
            root.path()
                .join(RESOURCE_SKILL)
                .join("SKILL.md")
                .to_str()
                .unwrap()
        );
        assert_eq!(files.len(), FILES.len());
        for (actual, &(path, bytes)) in files.iter().zip(FILES) {
            assert_eq!(actual.path, path);
            assert_eq!(actual.content.as_bytes(), bytes);
        }
        assert_eq!(
            serde_json::to_value(ClientSkill::Unavailable).unwrap(),
            serde_json::json!({"status":"unavailable"})
        );
    }
    #[test]
    fn missing_changed_and_oversized_resources_fail_closed() {
        let root = fixture();
        let file = root.path().join(RESOURCE_SKILL).join(FILES[0].0);
        std::fs::remove_file(&file).unwrap();
        assert!(packaged_skill(root.path(), "v").is_none());
        let mut changed = FILES[0].1.to_vec();
        changed[0] ^= 1;
        std::fs::write(&file, changed).unwrap();
        assert!(packaged_skill(root.path(), "v").is_none());
        let oversized = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        oversized.set_len(16 * 1024 * 1024).unwrap();
        assert!(packaged_skill(root.path(), "v").is_none());
    }
    #[cfg(unix)]
    #[test]
    fn file_and_directory_symlink_escapes_fail_closed_even_with_matching_bytes() {
        use std::os::unix::fs::symlink;
        let root = fixture();
        let outside = tempfile::tempdir().unwrap();
        let file = root.path().join(RESOURCE_SKILL).join("SKILL.md");
        std::fs::write(outside.path().join("SKILL.md"), FILES[0].1).unwrap();
        std::fs::remove_file(&file).unwrap();
        symlink(outside.path().join("SKILL.md"), &file).unwrap();
        assert!(packaged_skill(root.path(), "v").is_none());
        std::fs::remove_file(&file).unwrap();
        std::fs::write(&file, FILES[0].1).unwrap();
        let refs = root.path().join(RESOURCE_SKILL).join("references");
        std::fs::rename(&refs, outside.path().join("references")).unwrap();
        symlink(outside.path().join("references"), refs).unwrap();
        assert!(packaged_skill(root.path(), "v").is_none());
    }
    #[test]
    fn bundle_maps_the_complete_canonical_tree_without_a_vendored_copy() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        assert_eq!(
            config["bundle"]["resources"]["../.agents/skills/read-cartograph-context/"],
            "skills/read-cartograph-context/"
        );
        let canonical =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../.agents/skills/read-cartograph-context");
        fn paths(root: &Path, dir: &Path, result: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    paths(root, &entry.path(), result);
                } else {
                    result.push(
                        entry
                            .path()
                            .strip_prefix(root)
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .replace('\\', "/"),
                    );
                }
            }
        }
        let mut actual = Vec::new();
        paths(&canonical, &canonical, &mut actual);
        actual.sort();
        let mut expected: Vec<_> = FILES.iter().map(|(path, _)| (*path).to_owned()).collect();
        expected.sort();
        assert_eq!(actual, expected);
        for &(path, expected) in FILES {
            assert_eq!(std::fs::read(canonical.join(path)).unwrap(), expected);
            // Every authored relative Markdown link must remain within the
            // packaged file set, including links added to reference documents.
            let text = std::str::from_utf8(expected).unwrap();
            for suffix in text.split("](").skip(1) {
                let target = suffix.split(')').next().unwrap().split('#').next().unwrap();
                if target.is_empty() || target.contains("://") {
                    continue;
                }
                let linked = std::fs::canonicalize(
                    canonical
                        .join(Path::new(path).parent().unwrap())
                        .join(target),
                )
                .unwrap();
                let canonical_root = std::fs::canonicalize(&canonical).unwrap();
                assert!(linked.starts_with(&canonical_root));
                assert!(
                    FILES
                        .iter()
                        .any(|(file, _)| canonical_root.join(file) == linked),
                    "unpackaged skill link: {path} -> {target}"
                );
            }
        }
    }
}
