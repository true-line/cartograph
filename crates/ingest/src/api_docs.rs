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

/// Most API reference documents a project may allow per repository (#1653,
/// AC-1804).
pub const HARD_MAX_API_DOCS: usize = 64;

/// Largest single API reference document a project may allow (8 MiB). Every
/// validated document is held in memory, so the two ceilings bound a
/// repository's API references to 512 MiB.
pub const HARD_MAX_API_DOC_BYTES: u64 = 8 * 1024 * 1024;

/// How many API reference documents a repository may declare and how large
/// each may be (#1653, AC-1804). [`Default`] is [`MAX_API_DOCS`] documents of
/// up to [`MAX_API_DOC_BYTES`]; a project may choose other bounds up to the
/// hard ceilings. The bound of `.cartograph/api-docs.toml` itself stays fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiDocLimits {
    max_docs: usize,
    max_doc_bytes: u64,
}

impl Default for ApiDocLimits {
    fn default() -> Self {
        Self {
            max_docs: MAX_API_DOCS,
            max_doc_bytes: MAX_API_DOC_BYTES,
        }
    }
}

impl ApiDocLimits {
    /// Bounds of `max_docs` documents of up to `max_doc_bytes` each, or
    /// `None` when either is zero or above its hard ceiling.
    pub fn within(max_docs: usize, max_doc_bytes: u64) -> Option<Self> {
        ((1..=HARD_MAX_API_DOCS).contains(&max_docs)
            && (1..=HARD_MAX_API_DOC_BYTES).contains(&max_doc_bytes))
        .then_some(Self {
            max_docs,
            max_doc_bytes,
        })
    }
}

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
    /// Raw UTF-8 file contents (within the loaded [`ApiDocLimits`]).
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

/// Why [`read_anchored`] refused or failed to read a repo-relative file.
pub(crate) enum AnchoredRead {
    /// A component of the path does not exist.
    NotFound,
    /// A component is a symlink or not a directory, or the final entry is not
    /// a regular file.
    NotRegular,
    /// The file holds more than the byte bound it was read under.
    TooLarge(u64),
    /// Any other I/O failure.
    Io(std::io::Error),
}

impl From<std::io::Error> for AnchoredRead {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Read the slash path `rel` beneath `root` without following any symlink or
/// re-resolving the path, so the bytes are read from the exact entry that was
/// checked. Each directory is opened by a single no-follow component. The file
/// is opened no-follow and nonblocking, so a FIFO swapped in cannot block, and
/// it is typed and size-checked on the opened handle. At most `max_bytes` + 1
/// bytes are read, so a file that grows after the check is still rejected.
/// This mirrors `source_capture`'s selected-file read.
pub(crate) fn read_anchored(
    root: &Path,
    rel: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, AnchoredRead> {
    use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
    use std::io::Read;
    let not_found = |err: std::io::Error| {
        if err.kind() == std::io::ErrorKind::NotFound {
            AnchoredRead::NotFound
        } else {
            AnchoredRead::NotRegular
        }
    };
    let mut directory = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    let mut components = rel.split('/').peekable();
    while let Some(component) = components.next() {
        if components.peek().is_some() {
            directory = directory.open_dir_nofollow(component).map_err(not_found)?;
            continue;
        }
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let file = directory
            .open_with(component, &options)
            .map_err(not_found)?;
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() {
            return Err(AnchoredRead::NotRegular);
        }
        if metadata.len() > max_bytes {
            return Err(AnchoredRead::TooLarge(max_bytes));
        }
        let mut bytes = Vec::new();
        file.take(max_bytes + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max_bytes {
            return Err(AnchoredRead::TooLarge(max_bytes));
        }
        return Ok(bytes);
    }
    Err(AnchoredRead::NotRegular)
}

/// Gap reason for a failed [`read_anchored`] of `what`.
pub(crate) fn anchored_reason(what: &str, failure: AnchoredRead) -> String {
    match failure {
        AnchoredRead::NotFound => format!("{what} does not exist"),
        AnchoredRead::NotRegular => {
            format!(
                "{what} must be a regular file inside the repository, with no symlink in its path"
            )
        }
        AnchoredRead::TooLarge(max_bytes) => format!("{what} exceeds the {max_bytes} byte limit"),
        AnchoredRead::Io(err) => format!("{what} cannot be read: {err}"),
    }
}

/// Load `.cartograph/api-docs.toml` (if present under `root`), union its entries
/// with `manifest_docs` (manifest wins on duplicate `path`), and validate every
/// declaration against `root`.
///
/// A malformed `.cartograph/api-docs.toml` yields one [`ApiDocValidationGap`]
/// and contributes no declarations; manifest declarations, an independent
/// source, are still validated and processed.
pub fn load_api_docs(root: &Path, manifest_docs: &[ApiDocDeclaration]) -> LoadedApiDocs {
    load_api_docs_within(root, manifest_docs, ApiDocLimits::default())
}

/// [`load_api_docs`] under a project's `limits` (#1653, AC-1804): more
/// declared documents than `limits` allows is one gap and no documents, and a
/// larger document is a gap of its own.
pub fn load_api_docs_within(
    root: &Path,
    manifest_docs: &[ApiDocDeclaration],
    limits: ApiDocLimits,
) -> LoadedApiDocs {
    load_from(&Checkout(root), manifest_docs, limits)
}

/// The files [`load_api_docs_within`] reads under `root` for
/// `manifest_docs`: `.cartograph/api-docs.toml` when it is read, and every
/// declared document, in path order. A source capture retains them
/// (AC-1067). `None` when the load names any gap: a refused declaration is
/// reported from the checkout, so its reason never depends on what a capture
/// left out.
pub fn api_doc_paths(
    root: &Path,
    manifest_docs: &[ApiDocDeclaration],
    limits: ApiDocLimits,
) -> Option<Vec<String>> {
    let loaded = load_api_docs_within(root, manifest_docs, limits);
    if !loaded.gaps.is_empty() {
        return None;
    }
    let mut paths: Vec<String> = loaded.docs.into_iter().map(|doc| doc.path).collect();
    let checkout = Checkout(root);
    if checkout.config_present() && checkout.read(API_DOCS_REL_PATH, MAX_API_DOC_BYTES).is_ok() {
        paths.push(API_DOCS_REL_PATH.to_string());
    }
    paths.sort();
    paths.dedup();
    Some(paths)
}

/// [`load_api_docs_within`] over a source capture (AC-1067, #385): `read`
/// gives a captured file's bytes, `None` for a path the capture does not
/// hold. The capture holds [`api_doc_paths`], so the declarations, documents
/// and contents are the captured ones: a declaration or document edited,
/// deleted or created in the checkout after capture changes nothing.
pub fn load_api_docs_from_capture(
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
    manifest_docs: &[ApiDocDeclaration],
    limits: ApiDocLimits,
) -> LoadedApiDocs {
    load_from(&Captured(read), manifest_docs, limits)
}

/// Where the loader's bytes come from.
trait Files {
    /// Whether `.cartograph` can hold a declaration: absent, or a plain
    /// file, means the repository never opted in.
    fn config_present(&self) -> bool;
    /// [`read_anchored`]'s answer for `rel`.
    fn read(&self, rel: &str, max_bytes: u64) -> Result<Vec<u8>, AnchoredRead>;
}

struct Checkout<'a>(&'a Path);

impl Files for Checkout<'_> {
    fn config_present(&self) -> bool {
        // A repository that never opted in may already use `.cartograph` as a
        // plain file; only a directory (or a symlink, refused by the read
        // without being followed) can hold a declaration.
        let config_dir = API_DOCS_REL_PATH
            .split('/')
            .next()
            .expect("config path has a first component");
        match cap_std::fs::Dir::open_ambient_dir(self.0, cap_std::ambient_authority())
            .and_then(|dir| dir.symlink_metadata(config_dir))
        {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
            Ok(meta) if !meta.is_dir() && !meta.file_type().is_symlink() => false,
            _ => true,
        }
    }
    fn read(&self, rel: &str, max_bytes: u64) -> Result<Vec<u8>, AnchoredRead> {
        read_anchored(self.0, rel, max_bytes)
    }
}

struct Captured<'a>(&'a dyn Fn(&str) -> Option<Vec<u8>>);

impl Files for Captured<'_> {
    fn config_present(&self) -> bool {
        (self.0)(API_DOCS_REL_PATH).is_some()
    }
    fn read(&self, rel: &str, max_bytes: u64) -> Result<Vec<u8>, AnchoredRead> {
        let bytes = (self.0)(rel).ok_or(AnchoredRead::NotFound)?;
        if bytes.len() as u64 > max_bytes {
            return Err(AnchoredRead::TooLarge(max_bytes));
        }
        Ok(bytes)
    }
}

fn load_from(
    files: &dyn Files,
    manifest_docs: &[ApiDocDeclaration],
    limits: ApiDocLimits,
) -> LoadedApiDocs {
    let (repo_docs, config_gap) = match load_repo_config(files) {
        Ok(docs) => (docs, None),
        Err(gap) => (Vec::new(), Some(gap)),
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
        return match config_gap {
            Some(gap) => LoadedApiDocs {
                declared: true,
                docs: Vec::new(),
                gaps: vec![gap],
            },
            None => LoadedApiDocs::default(),
        };
    }

    let max_count = merged.len().max(repo_docs.len()).max(manifest_docs.len());
    if max_count > limits.max_docs {
        return LoadedApiDocs {
            declared: true,
            docs: Vec::new(),
            gaps: config_gap
                .into_iter()
                .chain([ApiDocValidationGap {
                    doc_path: API_DOCS_REL_PATH.into(),
                    reason: format!(
                        "declared {max_count} API reference docs; at most {} are allowed",
                        limits.max_docs
                    ),
                }])
                .collect(),
        };
    }

    let mut docs = Vec::new();
    let mut gaps: Vec<ApiDocValidationGap> = config_gap.into_iter().collect();
    for decl in &merged {
        match validate_one_doc(files, decl, limits.max_doc_bytes) {
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

fn load_repo_config(files: &dyn Files) -> Result<Vec<ApiDocDeclaration>, ApiDocValidationGap> {
    let gap = |reason: String| ApiDocValidationGap {
        doc_path: API_DOCS_REL_PATH.into(),
        reason,
    };
    if !files.config_present() {
        return Ok(Vec::new());
    }
    let bytes = match files.read(API_DOCS_REL_PATH, MAX_API_DOC_BYTES) {
        Ok(bytes) => bytes,
        Err(AnchoredRead::NotFound) => return Ok(Vec::new()),
        Err(failure) => {
            return Err(gap(format!(
                "invalid {}",
                anchored_reason(API_DOCS_REL_PATH, failure)
            )));
        }
    };
    let raw = String::from_utf8(bytes).map_err(|err| {
        gap(format!(
            "invalid {API_DOCS_REL_PATH}: not valid UTF-8 ({err})"
        ))
    })?;
    let parsed: ApiDocsConfig =
        toml::from_str(&raw).map_err(|err| gap(format!("malformed {API_DOCS_REL_PATH}: {err}")))?;
    Ok(parsed.doc)
}

fn validate_one_doc(
    files: &dyn Files,
    decl: &ApiDocDeclaration,
    max_doc_bytes: u64,
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
                norm_parts.push(part_str.to_string());
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
    let bytes = files
        .read(&norm_path, max_doc_bytes)
        .map_err(|failure| ApiDocValidationGap {
            doc_path: norm_path.clone(),
            reason: anchored_reason(&format!("declared API doc \"{norm_path}\""), failure),
        })?;
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

        // 6. Malformed `.cartograph/api-docs.toml` produces one Gap and no repo
        // declarations; the independent manifest declarations still load.
        std::fs::write(root.join(API_DOCS_REL_PATH), "[[doc]\npath = 123\n").unwrap();
        let malformed = load_api_docs(
            root,
            &[ApiDocDeclaration {
                path: "docs/api.md".into(),
                prefix: Some("/api".into()),
            }],
        );
        assert!(malformed.declared);
        assert_eq!(malformed.gaps.len(), 1);
        assert!(malformed.gaps[0].reason.contains("malformed"));
        assert_eq!(malformed.docs.len(), 1, "{:?}", malformed.docs);
        assert_eq!(malformed.docs[0].path, "docs/api.md");
        assert_eq!(malformed.docs[0].prefix.as_deref(), Some("/api"));

        // 7. A `.cartograph` regular file is not an opt-in: no declaration and
        // no Gap, so the drift register of a non-participating repo is unchanged.
        let plain = tempfile::tempdir().unwrap();
        std::fs::write(plain.path().join(".cartograph"), "unrelated tool state\n").unwrap();
        let not_declared = load_api_docs(plain.path(), &[]);
        assert!(!not_declared.declared);
        assert!(not_declared.docs.is_empty() && not_declared.gaps.is_empty());
    }

    // AC-1804 (#1653): a project's bounds decide how many API references a
    // repository may declare and how large each may be; the defaults keep
    // 16 documents of up to 1 MiB, and neither bound passes its ceiling.
    #[test]
    fn api_doc_limits_bound_count_and_size_within_the_hard_ceilings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        let declared = (0..17)
            .map(|n| {
                let path = format!("docs/api{n}.md");
                std::fs::write(root.join(&path), format!("#### GET /api/v{n}\n")).unwrap();
                ApiDocDeclaration { path, prefix: None }
            })
            .collect::<Vec<_>>();
        let default = load_api_docs(root, &declared);
        assert!(default.docs.is_empty());
        assert_eq!(
            default.gaps[0].reason,
            "declared 17 API reference docs; at most 16 are allowed"
        );
        let raised = ApiDocLimits::within(17, MAX_API_DOC_BYTES).unwrap();
        let read = load_api_docs_within(root, &declared, raised);
        assert_eq!(read.docs.len(), 17);
        assert!(read.gaps.is_empty());

        let big = MAX_API_DOC_BYTES as usize + 1;
        std::fs::write(root.join("docs/big.md"), vec![b'a'; big]).unwrap();
        let one = [ApiDocDeclaration {
            path: "docs/big.md".into(),
            prefix: None,
        }];
        let refused = load_api_docs(root, &one);
        assert!(refused.docs.is_empty());
        assert!(
            refused.gaps[0]
                .reason
                .ends_with("exceeds the 1048576 byte limit")
        );
        let roomy = ApiDocLimits::within(MAX_API_DOCS, 2 * MAX_API_DOC_BYTES).unwrap();
        assert_eq!(
            load_api_docs_within(root, &one, roomy).docs[0]
                .contents
                .len(),
            big
        );
        let tight = ApiDocLimits::within(MAX_API_DOCS, 8).unwrap();
        let small = [ApiDocDeclaration {
            path: "docs/api0.md".into(),
            prefix: None,
        }];
        assert!(
            load_api_docs_within(root, &small, tight).gaps[0]
                .reason
                .ends_with("exceeds the 8 byte limit")
        );

        assert_eq!(
            ApiDocLimits::default(),
            ApiDocLimits::within(16, 1 << 20).unwrap()
        );
        assert!(ApiDocLimits::within(0, MAX_API_DOC_BYTES).is_none());
        assert!(ApiDocLimits::within(HARD_MAX_API_DOCS + 1, MAX_API_DOC_BYTES).is_none());
        assert!(ApiDocLimits::within(MAX_API_DOCS, 0).is_none());
        assert!(ApiDocLimits::within(MAX_API_DOCS, HARD_MAX_API_DOC_BYTES + 1).is_none());
        assert!(ApiDocLimits::within(HARD_MAX_API_DOCS, HARD_MAX_API_DOC_BYTES).is_some());
    }

    // AC-0313 (#739): reads are anchored at the opened handle, so a FIFO at the
    // declared path fails closed without blocking, and a symlinked ancestor or
    // `.cartograph` directory is refused rather than followed out of the root.
    #[cfg(unix)]
    #[test]
    fn api_doc_reads_refuse_fifos_and_symlinked_ancestors_without_following() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("api.md"), "#### GET /api/escape\n").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        let fifo = root.join("docs/fifo.md");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(status.success());
        std::os::unix::fs::symlink(outside.path(), root.join("linked")).unwrap();
        let declared = |path: &str| ApiDocDeclaration {
            path: path.into(),
            prefix: None,
        };
        let loaded = load_api_docs(root, &[declared("docs/fifo.md"), declared("linked/api.md")]);
        assert!(loaded.docs.is_empty(), "{:?}", loaded.docs);
        assert_eq!(loaded.gaps.len(), 2, "{:?}", loaded.gaps);
        for gap in &loaded.gaps {
            assert!(gap.reason.contains("no symlink in its path"), "{gap:?}");
        }

        let config_root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(outside.path().join("cfg")).unwrap();
        std::fs::write(
            outside.path().join("cfg/api-docs.toml"),
            "[[doc]]\npath = \"api.md\"\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("cfg"),
            config_root.path().join(".cartograph"),
        )
        .unwrap();
        let refused = load_api_docs(config_root.path(), &[]);
        assert!(refused.declared && refused.docs.is_empty());
        assert_eq!(refused.gaps.len(), 1);
        assert!(
            refused.gaps[0].reason.contains("no symlink in its path"),
            "{:?}",
            refused.gaps
        );
    }

    // AC-1067 (T-1067, #385): over a source capture the loader reads the
    // captured config and documents and never the checkout: a declaration or
    // document edited, deleted or created after capture changes nothing. A
    // load with any gap selects nothing, so its reasons stay the checkout's.
    #[test]
    fn captured_api_docs_are_read_instead_of_the_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".cartograph")).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/api.md"), "#### GET /api/items\n").unwrap();
        std::fs::write(root.join("docs/extra.md"), "#### POST /api/extra\n").unwrap();
        std::fs::write(root.join("docs/unused.md"), "#### GET /api/unused\n").unwrap();
        std::fs::write(
            root.join(API_DOCS_REL_PATH),
            "[[doc]]\npath = \"docs/api.md\"\nprefix = \"/api\"\n",
        )
        .unwrap();
        let manifest = [ApiDocDeclaration {
            path: "docs/extra.md".into(),
            prefix: None,
        }];
        let limits = ApiDocLimits::default();
        let before = load_api_docs_within(root, &manifest, limits);
        assert_eq!(before.docs.len(), 2);

        let paths = api_doc_paths(root, &manifest, limits).unwrap();
        assert_eq!(paths, [API_DOCS_REL_PATH, "docs/api.md", "docs/extra.md"]);
        let captured: std::collections::BTreeMap<String, Vec<u8>> = paths
            .iter()
            .map(|rel| (rel.clone(), std::fs::read(root.join(rel)).unwrap()))
            .collect();

        // After capture: edits, a deletion and a new declaration.
        std::fs::write(root.join("docs/api.md"), "#### GET /api/other\n").unwrap();
        std::fs::write(
            root.join(API_DOCS_REL_PATH),
            "[[doc]]\npath = \"docs/api.md\"\n[[doc]]\npath = \"docs/unused.md\"\n",
        )
        .unwrap();
        std::fs::remove_file(root.join("docs/extra.md")).unwrap();
        let read = |rel: &str| captured.get(rel).cloned();
        assert_eq!(load_api_docs_from_capture(&read, &manifest, limits), before);
        assert_ne!(load_api_docs_within(root, &manifest, limits), before);

        // The deleted document is now a gap: nothing is selected.
        assert_eq!(api_doc_paths(root, &manifest, limits), None);
        // A repository that never opted in selects nothing to retain.
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(api_doc_paths(plain.path(), &[], limits), Some(Vec::new()));
        let none = |_: &str| None;
        assert_eq!(
            load_api_docs_from_capture(&none, &[], limits),
            load_api_docs_within(plain.path(), &[], limits)
        );
    }
}
