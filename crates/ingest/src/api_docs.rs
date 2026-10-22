//! Author-declared API reference document loader and fail-closed validator (#739, AC-0313).
//!
//! API contract docs are never auto-discovered. A repository may declare up to
//! [`MAX_API_DOCS`] references in `.cartograph/api-docs.toml` at its root, and a
//! system manifest (`cartograph.system.toml`) may declare per-repo `api_docs`
//! entries using the same [`ApiDocDeclaration`] schema. Declarations from both
//! sources are unioned by normalized `path`, with the manifest entry winning on
//! duplicates.
//!
//! Every validation failure — malformed `.cartograph/api-docs.toml`, more than
//! [`MAX_API_DOCS`] documents, non-relative or `..` paths, symlinks, files
//! outside the canonical repository root, files larger than [`MAX_API_DOC_BYTES`],
//! or prefixes that do not start with `/` — is returned as an explicit
//! [`ApiDocValidationGap`] and never aborts repository recovery.

use std::path::{Component, Path};

/// Canonical repo-local API docs configuration path.
pub const API_DOCS_REL_PATH: &str = ".cartograph/api-docs.toml";

/// Maximum number of declared API reference documents per repository.
pub const MAX_API_DOCS: usize = 16;

/// Maximum byte size of a single declared API reference document (1 MiB).
pub const MAX_API_DOC_BYTES: u64 = 1024 * 1024;

/// One author-declared API reference document (#739, AC-0313).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApiDocDeclaration {
    /// Repo-relative path to the API reference file (`.md` or OpenAPI 3 `.json`).
    pub path: String,
    /// Optional URL path prefix (must start with `/`) scoping `served-undocumented` checks.
    #[serde(default)]
    pub prefix: Option<String>,
}

/// Top-level schema of `.cartograph/api-docs.toml`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiDocsConfig {
    /// Declared API reference documents (`[[doc]]`).
    #[serde(default)]
    doc: Vec<ApiDocDeclaration>,
}

/// A validated, ready-to-parse API reference document inside the repository root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedApiDoc {
    /// Normalized repo-relative slash path (e.g. `docs/api.md`).
    pub path: String,
    /// Optional URL path prefix (guaranteed to start with `/`).
    pub prefix: Option<String>,
    /// Raw UTF-8 file contents (at most [`MAX_API_DOC_BYTES`] bytes).
    pub contents: String,
}

/// A fail-closed validation gap produced while loading API doc declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiDocValidationGap {
    /// Declared path (or `.cartograph/api-docs.toml` for config-level failures).
    pub doc_path: String,
    /// Human-readable explanation of why the declaration was rejected.
    pub reason: String,
}

/// Outcome of loading and validating a repository's API doc declarations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedApiDocs {
    /// True when at least one API doc was declared or `.cartograph/api-docs.toml` failed validation.
    pub declared: bool,
    /// Validated documents ready for route extraction.
    pub docs: Vec<ValidatedApiDoc>,
    /// Fail-closed validation gaps (never a recovery failure).
    pub gaps: Vec<ApiDocValidationGap>,
}

/// Declared path with empty and `.` segments removed, so `./docs//api.md` and
/// `docs/api.md` are one declaration (the validated path is built the same way).
fn normalize_key(path: &str) -> String {
    path.split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// Read at most [`MAX_API_DOC_BYTES`] + 1 bytes from `path`, so a file that grows
/// after its metadata was checked is still rejected without reading it whole.
fn read_bounded(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_API_DOC_BYTES + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Load `.cartograph/api-docs.toml` (if present under `root`), union its entries
/// with `manifest_docs` (manifest wins on duplicate `path`), and validate every
/// declaration against `root`.
///
/// A malformed `.cartograph/api-docs.toml` yields one [`ApiDocValidationGap`]
/// and no validated documents.
pub fn load_api_docs(root: &Path, manifest_docs: &[ApiDocDeclaration]) -> LoadedApiDocs {
    let repo_docs = match load_repo_config(root) {
        Ok(docs) => docs,
        Err(gap) => {
            return LoadedApiDocs {
                declared: true,
                docs: Vec::new(),
                gaps: vec![gap],
            };
        }
    };

    let mut merged: Vec<ApiDocDeclaration> = Vec::new();
    for decl in &repo_docs {
        let key = normalize_key(&decl.path);
        if let Some(existing) = merged.iter_mut().find(|d| normalize_key(&d.path) == key) {
            *existing = decl.clone();
        } else {
            merged.push(decl.clone());
        }
    }
    for decl in manifest_docs {
        let key = normalize_key(&decl.path);
        if let Some(existing) = merged.iter_mut().find(|d| normalize_key(&d.path) == key) {
            *existing = decl.clone();
        } else {
            merged.push(decl.clone());
        }
    }

    if merged.is_empty() {
        return LoadedApiDocs::default();
    }

    let max_count = merged.len().max(repo_docs.len()).max(manifest_docs.len());
    if max_count > MAX_API_DOCS {
        return LoadedApiDocs {
            declared: true,
            docs: Vec::new(),
            gaps: vec![ApiDocValidationGap {
                doc_path: API_DOCS_REL_PATH.into(),
                reason: format!(
                    "declared {max_count} API reference docs; at most {MAX_API_DOCS} are allowed"
                ),
            }],
        };
    }

    let canon_root = match dunce::canonicalize(root) {
        Ok(canon) => canon,
        Err(err) => {
            return LoadedApiDocs {
                declared: true,
                docs: Vec::new(),
                gaps: vec![ApiDocValidationGap {
                    doc_path: API_DOCS_REL_PATH.into(),
                    reason: format!("cannot canonicalize repository root: {err}"),
                }],
            };
        }
    };

    let mut docs = Vec::new();
    let mut gaps = Vec::new();
    for decl in &merged {
        match validate_one_doc(root, &canon_root, decl) {
            Ok(valid) => docs.push(valid),
            Err(gap) => gaps.push(gap),
        }
    }

    LoadedApiDocs {
        declared: true,
        docs,
        gaps,
    }
}

fn load_repo_config(root: &Path) -> Result<Vec<ApiDocDeclaration>, ApiDocValidationGap> {
    let dot_cartograph = root.join(".cartograph");
    match std::fs::symlink_metadata(&dot_cartograph) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(ApiDocValidationGap {
                doc_path: API_DOCS_REL_PATH.into(),
                reason: format!("cannot inspect .cartograph directory: {err}"),
            });
        }
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
                return Err(ApiDocValidationGap {
                    doc_path: API_DOCS_REL_PATH.into(),
                    reason: "invalid .cartograph/api-docs.toml: .cartograph must be a real directory, not a symlink".into(),
                });
            }
        }
    }

    let cfg_path = root.join(API_DOCS_REL_PATH);
    let meta = match std::fs::symlink_metadata(&cfg_path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(ApiDocValidationGap {
                doc_path: API_DOCS_REL_PATH.into(),
                reason: format!("cannot inspect {API_DOCS_REL_PATH}: {err}"),
            });
        }
        Ok(meta) => meta,
    };

    if meta.file_type().is_symlink() || !meta.file_type().is_file() {
        return Err(ApiDocValidationGap {
            doc_path: API_DOCS_REL_PATH.into(),
            reason: format!("invalid {API_DOCS_REL_PATH}: must be a regular file, not a symlink"),
        });
    }
    if meta.len() > MAX_API_DOC_BYTES {
        return Err(ApiDocValidationGap {
            doc_path: API_DOCS_REL_PATH.into(),
            reason: format!(
                "invalid {API_DOCS_REL_PATH}: {} bytes exceeds {MAX_API_DOC_BYTES} byte limit",
                meta.len()
            ),
        });
    }

    let bytes = read_bounded(&cfg_path).map_err(|err| ApiDocValidationGap {
        doc_path: API_DOCS_REL_PATH.into(),
        reason: format!("cannot read {API_DOCS_REL_PATH}: {err}"),
    })?;
    if bytes.len() as u64 > MAX_API_DOC_BYTES {
        return Err(ApiDocValidationGap {
            doc_path: API_DOCS_REL_PATH.into(),
            reason: format!(
                "invalid {API_DOCS_REL_PATH}: {} bytes exceeds {MAX_API_DOC_BYTES} byte limit",
                bytes.len()
            ),
        });
    }
    let raw = String::from_utf8(bytes).map_err(|err| ApiDocValidationGap {
        doc_path: API_DOCS_REL_PATH.into(),
        reason: format!("invalid {API_DOCS_REL_PATH}: not valid UTF-8 ({err})"),
    })?;
    let parsed: ApiDocsConfig = toml::from_str(&raw).map_err(|err| ApiDocValidationGap {
        doc_path: API_DOCS_REL_PATH.into(),
        reason: format!("malformed {API_DOCS_REL_PATH}: {err}"),
    })?;
    Ok(parsed.doc)
}

fn validate_one_doc(
    root: &Path,
    canon_root: &Path,
    decl: &ApiDocDeclaration,
) -> Result<ValidatedApiDoc, ApiDocValidationGap> {
    let raw_path = decl.path.as_str();
    if raw_path.is_empty() {
        return Err(ApiDocValidationGap {
            doc_path: raw_path.into(),
            reason: "declared API doc path must not be empty".into(),
        });
    }
    if raw_path.starts_with('/')
        || raw_path.starts_with('\\')
        || Path::new(raw_path).is_absolute()
        || raw_path.contains('\\')
    {
        return Err(ApiDocValidationGap {
            doc_path: raw_path.into(),
            reason: format!(
                "declared API doc path \"{raw_path}\" must be a relative slash path, not absolute"
            ),
        });
    }
    if raw_path.split('/').any(|seg| seg == "..")
        || Path::new(raw_path).components().any(|comp| {
            matches!(
                comp,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ApiDocValidationGap {
            doc_path: raw_path.into(),
            reason: format!("declared API doc path \"{raw_path}\" must not contain '..'"),
        });
    }

    if let Some(prefix) = &decl.prefix
        && !prefix.starts_with('/')
    {
        return Err(ApiDocValidationGap {
            doc_path: raw_path.into(),
            reason: format!(
                "declared API doc prefix \"{prefix}\" for \"{raw_path}\" must start with '/'"
            ),
        });
    }

    let mut current = root.to_path_buf();
    let mut norm_parts = Vec::new();
    for comp in Path::new(raw_path).components() {
        match comp {
            Component::Normal(part) => {
                let Some(part_str) = part.to_str() else {
                    return Err(ApiDocValidationGap {
                        doc_path: raw_path.into(),
                        reason: format!("declared API doc path \"{raw_path}\" is not valid UTF-8"),
                    });
                };
                current.push(part);
                norm_parts.push(part_str.to_string());
                let meta =
                    std::fs::symlink_metadata(&current).map_err(|err| ApiDocValidationGap {
                        doc_path: raw_path.into(),
                        reason: format!("declared API doc \"{raw_path}\" cannot be read: {err}"),
                    })?;
                if meta.file_type().is_symlink() {
                    return Err(ApiDocValidationGap {
                        doc_path: raw_path.into(),
                        reason: format!(
                            "declared API doc \"{raw_path}\" must not be a symlink or traverse a symlink"
                        ),
                    });
                }
            }
            Component::CurDir => {}
            _ => {
                return Err(ApiDocValidationGap {
                    doc_path: raw_path.into(),
                    reason: format!(
                        "declared API doc path \"{raw_path}\" must be a relative path inside the repository"
                    ),
                });
            }
        }
    }

    if norm_parts.is_empty() {
        return Err(ApiDocValidationGap {
            doc_path: raw_path.into(),
            reason: format!("declared API doc path \"{raw_path}\" must name a regular file"),
        });
    }

    let norm_path = norm_parts.join("/");
    let meta = std::fs::symlink_metadata(&current).map_err(|err| ApiDocValidationGap {
        doc_path: norm_path.clone(),
        reason: format!("declared API doc \"{norm_path}\" cannot be read: {err}"),
    })?;
    if meta.file_type().is_symlink() || !meta.file_type().is_file() {
        return Err(ApiDocValidationGap {
            doc_path: norm_path.clone(),
            reason: format!(
                "declared API doc \"{norm_path}\" must be a regular file, not a symlink"
            ),
        });
    }
    if meta.len() > MAX_API_DOC_BYTES {
        return Err(ApiDocValidationGap {
            doc_path: norm_path.clone(),
            reason: format!(
                "declared API doc \"{norm_path}\" is {} bytes (exceeds {MAX_API_DOC_BYTES} byte limit)",
                meta.len()
            ),
        });
    }

    let canon_file = dunce::canonicalize(&current).map_err(|err| ApiDocValidationGap {
        doc_path: norm_path.clone(),
        reason: format!("declared API doc \"{norm_path}\" cannot be canonicalized: {err}"),
    })?;
    if !canon_file.starts_with(canon_root) || canon_file == *canon_root {
        return Err(ApiDocValidationGap {
            doc_path: norm_path.clone(),
            reason: format!(
                "declared API doc \"{norm_path}\" resolves outside the repository root"
            ),
        });
    }

    let bytes = read_bounded(&canon_file).map_err(|err| ApiDocValidationGap {
        doc_path: norm_path.clone(),
        reason: format!("declared API doc \"{norm_path}\" cannot be read: {err}"),
    })?;
    if bytes.len() as u64 > MAX_API_DOC_BYTES {
        return Err(ApiDocValidationGap {
            doc_path: norm_path.clone(),
            reason: format!(
                "declared API doc \"{norm_path}\" is {} bytes (exceeds {MAX_API_DOC_BYTES} byte limit)",
                bytes.len()
            ),
        });
    }
    let contents = String::from_utf8(bytes).map_err(|err| ApiDocValidationGap {
        doc_path: norm_path.clone(),
        reason: format!("declared API doc \"{norm_path}\" is not valid UTF-8: {err}"),
    })?;

    Ok(ValidatedApiDoc {
        path: norm_path,
        prefix: decl.prefix.clone(),
        contents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{MANIFEST_NAME, SystemManifest};

    // AC-0313 (#739): declaration validation rejects absolute paths, `..`,
    // symlinks, oversize files, >16 docs, non-`/` prefixes, and malformed TOML
    // as explicit Gaps without failing recovery; manifest entries win on
    // duplicate `path` in the union.
    #[test]
    fn api_doc_declarations_validate_paths_limits_and_manifest_union() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".cartograph")).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/api.md"), "#### GET /api/items\n").unwrap();
        std::fs::write(root.join("docs/extra.md"), "#### POST /api/extra\n").unwrap();

        // 1. Union with manifest winning on duplicate path, `./` and `//` ignored.
        std::fs::write(
            root.join(API_DOCS_REL_PATH),
            r#"
[[doc]]
path = "docs/api.md"
prefix = "/legacy"

[[doc]]
path = "docs/extra.md"
"#,
        )
        .unwrap();

        let manifest_file = root.join(MANIFEST_NAME);
        std::fs::write(
            &manifest_file,
            r#"
[[repos]]
url = "acme/api"

[[repos.api_docs]]
path = "./docs//api.md"
prefix = "/api"
"#,
        )
        .unwrap();
        let manifest = SystemManifest::load(&manifest_file).unwrap();
        let loaded = load_api_docs(root, &manifest.repos[0].api_docs);
        assert!(loaded.declared);
        assert!(loaded.gaps.is_empty(), "{:?}", loaded.gaps);
        assert_eq!(loaded.docs.len(), 2);
        assert_eq!(loaded.docs[0].path, "docs/api.md");
        assert_eq!(loaded.docs[0].prefix.as_deref(), Some("/api"));
        assert_eq!(loaded.docs[1].path, "docs/extra.md");
        assert_eq!(loaded.docs[1].prefix, None);

        // 2. Absolute path, `..`, and prefix without leading `/` produce Gaps.
        std::fs::write(
            root.join(API_DOCS_REL_PATH),
            r#"
[[doc]]
path = "/etc/passwd"

[[doc]]
path = "../outside.md"

[[doc]]
path = "docs/../api.md"

[[doc]]
path = "docs/api.md"
prefix = "api"
"#,
        )
        .unwrap();
        let bad = load_api_docs(root, &[]);
        assert!(bad.declared);
        assert!(bad.docs.is_empty());
        assert_eq!(bad.gaps.len(), 4);
        assert!(bad.gaps[0].reason.contains("relative"));
        assert!(bad.gaps[1].reason.contains(".."));
        assert!(bad.gaps[2].reason.contains(".."));
        assert!(bad.gaps[3].reason.contains("start with '/'"));

        // 3. Oversize (> 1 MiB) produces a Gap.
        let big_path = root.join("docs/huge.md");
        std::fs::write(&big_path, vec![b'a'; (MAX_API_DOC_BYTES as usize) + 1]).unwrap();
        std::fs::write(
            root.join(API_DOCS_REL_PATH),
            "[[doc]]\npath = \"docs/huge.md\"\n",
        )
        .unwrap();
        let oversize = load_api_docs(root, &[]);
        assert!(oversize.declared);
        assert!(oversize.docs.is_empty());
        assert_eq!(oversize.gaps.len(), 1);
        assert!(oversize.gaps[0].reason.contains("exceeds"));

        // 4. Symlink produces a Gap.
        #[cfg(unix)]
        {
            let link_path = root.join("docs/link.md");
            std::os::unix::fs::symlink(root.join("docs/api.md"), &link_path).unwrap();
            std::fs::write(
                root.join(API_DOCS_REL_PATH),
                "[[doc]]\npath = \"docs/link.md\"\n",
            )
            .unwrap();
            let symlinked = load_api_docs(root, &[]);
            assert!(symlinked.declared);
            assert!(symlinked.docs.is_empty());
            assert_eq!(symlinked.gaps.len(), 1);
            assert!(symlinked.gaps[0].reason.contains("symlink"));
        }

        // 5. > 16 docs produces one Gap and no contract facts.
        let mut seventeen = String::new();
        for i in 0..17 {
            let rel = format!("docs/d{i}.md");
            std::fs::write(root.join(&rel), "#### GET /api/v1\n").unwrap();
            seventeen.push_str(&format!("[[doc]]\npath = \"{rel}\"\n"));
        }
        std::fs::write(root.join(API_DOCS_REL_PATH), seventeen).unwrap();
        let too_many = load_api_docs(root, &[]);
        assert!(too_many.declared);
        assert!(too_many.docs.is_empty());
        assert_eq!(too_many.gaps.len(), 1);
        assert!(too_many.gaps[0].reason.contains("at most 16"));

        // 6. Malformed `.cartograph/api-docs.toml` produces one Gap and no docs,
        // even when manifest entries exist.
        std::fs::write(root.join(API_DOCS_REL_PATH), "[[doc]\npath = 123\n").unwrap();
        let malformed = load_api_docs(
            root,
            &[ApiDocDeclaration {
                path: "docs/api.md".into(),
                prefix: Some("/api".into()),
            }],
        );
        assert!(malformed.declared);
        assert!(malformed.docs.is_empty());
        assert_eq!(malformed.gaps.len(), 1);
        assert!(malformed.gaps[0].reason.contains("malformed"));
    }
}
