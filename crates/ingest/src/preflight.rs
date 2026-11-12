//! Local-only preflight: detect languages, frameworks, and adapter coverage,
//! and classify risky constructs **before** recovery (#116, AC-0055/AC-0059).
//!
//! T0 discipline: a pure filesystem scan — zero egress, never an LLM. The
//! product's honesty contract starts here with the three-way classification:
//!
//! - **Potential system gaps** — evidence exists but is not statically
//!   resolvable (dynamic injection, runtime-computed identity). These become
//!   explicit Gaps with resolution strategies after recovery, never a guess.
//! - **Unsupported patterns** — no adapter covers the construct. A tool
//!   limitation, **not** a System Gap; the two are never conflated.
//!
//! Detectors are a versioned registry ([`DETECTOR_ID`]): provenance on every
//! finding records which detector produced it, so re-runs are attributable.

use crate::adapter_lookup::{CellLookup, PluginDeclaration, builtin_claims, lookup_coverage};
use crate::matrix::{ReportedCell, cells_for};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Versioned identity of this detector registry, recorded on every finding.
pub const DETECTOR_ID: &str = "preflight@1";

/// Directories that are never part of the recovered system. `.cartograph`
/// holds Cartograph's own plugin artifacts/corpora — never project source
/// (a dropped-in `.wasm` plugin must not surface as an uncovered language).
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".venv",
    ".cartograph",
];

/// Shared skip predicate — the toolchain walk (#215) must never disagree
/// with Preflight about what is part of the system.
pub(crate) fn skip_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

/// One detected language with its adapter coverage.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LanguageDetection {
    /// Language name, e.g. `TypeScript`.
    pub language: String,
    /// Source files found.
    pub files: u64,
    /// Covering adapter id, or `None` when no adapter exists — in which case
    /// the whole language surfaces as an unsupported pattern.
    pub adapter: Option<String>,
    /// Coverage-matrix cells of the covering adapter: compiled-in cells
    /// (AC-0253) or a gated plugin's declared cells (AC-0263); empty for an
    /// uncovered language.
    pub coverage: Vec<ReportedCell>,
}

/// One classified construct: where it is and why it was flagged.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PatternFinding {
    /// Stable finding kind, e.g. `inline-eval`, `dynamic-injection`.
    pub kind: String,
    /// Repo-relative path.
    pub path: String,
    /// 1-based line of the first occurrence.
    pub line: u64,
    /// Human explanation shown in the Preflight card.
    pub message: String,
    /// Detector registry version that produced this finding.
    pub detector: String,
    /// For uncovered-language findings: the language to request an adapter
    /// for — the UI's "request adapter" lane (#201). `None` otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_adapter: Option<String>,
}

/// The full local detection report for one tree.
#[derive(Debug, Clone, Serialize, Default)]
pub struct PreflightReport {
    /// Languages by source-file extension, with adapter coverage.
    pub languages: Vec<LanguageDetection>,
    /// Frameworks/manifests recognized from marker files.
    pub frameworks: Vec<String>,
    /// Constructs no adapter covers — tool limitations, never Gaps.
    pub unsupported: Vec<PatternFinding>,
    /// Constructs that will become explicit Gaps if recovery cannot resolve
    /// them deterministically.
    pub potential_gaps: Vec<PatternFinding>,
    /// Detector registry version used for this report.
    pub detector: String,
    /// For each unsupported or partial cell above, which installed source
    /// declares coverage of it (#1071). Built-in claims and gated plugins
    /// only; a host with ungated plugins calls [`Self::refresh_adapter_lookup`].
    pub adapter_lookup: Vec<CellLookup>,
}

impl PreflightReport {
    /// Recompute [`Self::adapter_lookup`] against `plugins`, which may
    /// include plugins whose gate has not passed (reported as declared but
    /// unverified). Reads declarations only.
    pub fn refresh_adapter_lookup(&mut self, plugins: &[PluginDeclaration]) {
        self.adapter_lookup = report_lookup(&self.languages, plugins);
    }
}

/// The lookup over every non-supported cell the detected languages report,
/// plus the core cells an uncovered language requires.
fn report_lookup(
    languages: &[LanguageDetection],
    plugins: &[PluginDeclaration],
) -> Vec<CellLookup> {
    let cells: Vec<ReportedCell> = languages
        .iter()
        .flat_map(|language| language.coverage.iter().cloned())
        .chain(crate::adapter_packet::uncovered_cells(languages))
        .collect();
    lookup_coverage(&cells, &builtin_claims(), plugins)
}

/// One installed deterministic adapter — the per-language/per-format
/// extractor that turns source into Confirmed facts. This single registry
/// drives Preflight coverage AND the Settings inventory (#163), so the two
/// can never disagree.
#[derive(Debug, Clone, Serialize)]
pub struct AdapterInfo {
    /// Extractor id as recorded in provenance (`extractor_id`).
    pub id: &'static str,
    /// Language or format the adapter covers.
    pub language: &'static str,
    /// Source-file extensions the adapter claims.
    pub extensions: &'static [&'static str],
    /// What the adapter actually extracts, in plain language.
    pub covers: &'static str,
}

/// Every adapter shipped in this build.
pub const INSTALLED_ADAPTERS: &[AdapterInfo] = &[
    AdapterInfo {
        id: "t0.adapter-ts",
        language: "TypeScript",
        extensions: &["ts", "tsx"],
        covers: "imports, call graph, Express and node:http endpoints, Next/React Router screens, \
                 chrome messaging channels, IndexedDB",
    },
    AdapterInfo {
        id: "t0.adapter-ts",
        language: "JavaScript",
        extensions: &["js", "jsx", "mjs", "cjs"],
        covers: "imports, call graph, Express and node:http endpoints, Next/React Router screens, \
                 chrome messaging channels, IndexedDB — the TypeScript \
                 crate's own grammar parses plain JS/JSX directly",
    },
    AdapterInfo {
        id: "t0.adapter-ts",
        language: "Vue",
        extensions: &["vue"],
        covers: "<script> and <script setup> blocks of single-file components, parsed \
                 as JavaScript/TypeScript (imports, call graph, HTTP client calls); \
                 templates and styles are not parsed",
    },
    AdapterInfo {
        id: "t0.webextension",
        language: "WebExtension",
        extensions: &[],
        covers: "manifest.json execution contexts (background, content scripts, \
                 popups), keyboard commands, permissions as grants, entry binding",
    },
    AdapterInfo {
        id: "t0.adapter-python",
        language: "Python",
        extensions: &["py"],
        covers: "imports, call graph, FastAPI/Flask endpoint registrations",
    },
    AdapterInfo {
        id: "t0.adapter-go",
        language: "Go",
        extensions: &["go"],
        covers: "imports, call graph, net/http, chi and gin endpoint registrations",
    },
    AdapterInfo {
        id: "t0.adapter-rust",
        language: "Rust",
        extensions: &["rs"],
        covers: "imports, call graph, axum and actix-web endpoint registrations, reqwest client calls",
    },
    AdapterInfo {
        id: "t0.adapter-java",
        language: "Java",
        extensions: &["java"],
        covers: "types, methods, call graph, Spring Web and JAX-RS endpoint annotations",
    },
    AdapterInfo {
        id: "t0.adapter-kotlin",
        language: "Kotlin",
        extensions: &["kt", "kts"],
        covers: "types, objects, functions (incl. extensions), call graph, \
                 Spring Web endpoint annotations",
    },
    AdapterInfo {
        id: "t0.iac-terraform",
        language: "Terraform",
        extensions: &["tf"],
        covers: "resource DAG, AWS capability edges (TRIGGERS/ROUTES/GRANTS)",
    },
];

/// A named adapter that does not exist yet. Adapters are per *language*,
/// never per toolchain version (a JDK bump is not a new adapter): the
/// grammar covers syntax across versions and version-specific constructs
/// degrade to explicit Unsupported findings.
#[derive(Debug, Clone, Serialize)]
pub struct PlannedAdapter {
    /// Language the future adapter would cover.
    pub language: &'static str,
    /// Extensions Preflight uses to detect (and name) the language today.
    pub extensions: &'static [&'static str],
}

/// The recommendation catalog (#163): languages Preflight can detect and
/// name as installable-later adapter types.
pub const PLANNED_ADAPTERS: &[PlannedAdapter] = &[
    PlannedAdapter {
        language: "C",
        extensions: &["c", "h"],
    },
    PlannedAdapter {
        language: "C++",
        extensions: &["cc", "cpp", "cxx", "hpp", "hh"],
    },
    PlannedAdapter {
        language: "Swift",
        extensions: &["swift"],
    },
    PlannedAdapter {
        language: "Objective-C",
        extensions: &["m", "mm"],
    },
];

/// Languages the deterministic tier covers today, keyed by extension.
fn adapter_for(extension: &str) -> Option<(&'static str, &'static str)> {
    INSTALLED_ADAPTERS
        .iter()
        .find(|adapter| adapter.extensions.contains(&extension))
        .map(|adapter| (adapter.language, adapter.id))
}

/// Source languages we can *name* but not extract — surfaced honestly as
/// uncovered rather than silently ignored. Planned adapter types come from
/// the recommendation catalog; the rest are named-only.
fn uncovered_language(extension: &str) -> Option<&'static str> {
    if let Some(planned) = PLANNED_ADAPTERS
        .iter()
        .find(|planned| planned.extensions.contains(&extension))
    {
        return Some(planned.language);
    }
    NAMED_ONLY
        .iter()
        .find(|(ext, _)| *ext == extension)
        .map(|(_, language)| *language)
}

/// Languages Preflight names without a catalog entry, by extension. Only
/// extensions that need no content heuristic: `.ex` (Elixir or Euphoria),
/// `.fs` (F#, GLSL, Forth or Filterscript) and `.pl` (Perl or Prolog) would
/// name a language the file may not be.
const NAMED_ONLY: &[(&str, &str)] = &[
    ("rb", "Ruby"),
    ("cs", "C#"),
    ("php", "PHP"),
    ("wasm", "WebAssembly"),
    ("scala", "Scala"),
    ("groovy", "Groovy"),
    ("dart", "Dart"),
    ("exs", "Elixir"),
    ("erl", "Erlang"),
    ("clj", "Clojure"),
    ("cljs", "Clojure"),
    ("cljc", "Clojure"),
    ("fsx", "F#"),
    ("vb", "Visual Basic"),
    ("hs", "Haskell"),
    ("lua", "Lua"),
];

/// Every extension Preflight attributes to `language` (case-insensitive),
/// sorted; a plugin-named language such as `.zig` yields its bare extension.
pub(crate) fn extensions_for_language(language: &str) -> Vec<String> {
    let mut out: Vec<String> = INSTALLED_ADAPTERS
        .iter()
        .filter(|adapter| adapter.language.eq_ignore_ascii_case(language))
        .flat_map(|adapter| adapter.extensions.iter())
        .chain(
            PLANNED_ADAPTERS
                .iter()
                .filter(|planned| planned.language.eq_ignore_ascii_case(language))
                .flat_map(|planned| planned.extensions.iter()),
        )
        .map(|ext| ext.to_string())
        .chain(
            NAMED_ONLY
                .iter()
                .filter(|(_, named)| named.eq_ignore_ascii_case(language))
                .map(|(ext, _)| ext.to_string()),
        )
        .collect();
    if let Some(bare) = language.strip_prefix('.') {
        out.push(bare.to_string());
    }
    out.sort();
    out.dedup();
    out
}

/// True when a `language` has a planned adapter in the catalog.
pub fn planned_adapter_for(language: &str) -> bool {
    PLANNED_ADAPTERS
        .iter()
        .any(|planned| planned.language == language)
}

/// A gated, enabled plugin adapter's coverage claim (#201): the extensions
/// its golden corpus declares. Only plugins that passed the conformance
/// gate for their exact bytes may claim coverage — the caller enforces
/// that; this type just carries the claim.
#[derive(Debug, Clone)]
pub struct PluginCoverage {
    /// The plugin id, reported as the covering adapter.
    pub plugin_id: String,
    /// The gated artifact's content hash, the plugin's version key.
    pub version: String,
    /// Extensions the corpus declares, without the leading dot.
    pub extensions: Vec<String>,
    /// Coverage cells the gated corpus declares (AC-0263).
    pub cells: Vec<ReportedCell>,
    /// Language names the corpus gives extensions the core cannot name.
    pub languages: std::collections::BTreeMap<String, String>,
}

/// The TS adapter's AST-level classification of one `eval(` site (#214).
/// Preflight's textual scan cannot itself prove an argument compile-time
/// known; the adapter that owns extraction supplies these claims (host-
/// filled, mirroring [`PluginCoverage`]), so the scan and the AST proof can
/// never disagree about which sites are covered.
#[derive(Debug, Clone)]
pub struct EvalSiteCoverage {
    /// Repo-relative path, matching this scan's finding paths.
    pub path: String,
    /// 1-based line of the call, matching this scan's finding lines.
    pub line: u64,
    /// What the adapter's argument proof established.
    pub proof: EvalProof,
}

/// Outcome of the adapter's eval-argument proof (#214).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalProof {
    /// Compile-time-proven literal — the adapter extracted its facts as
    /// Confirmed T0, so the `inline-eval` finding is covered and closes
    /// (the #201 finding-closure semantics).
    Covered,
    /// Const-shaped but unproven — downgraded to an explicit potential Gap.
    ConstUnproven,
    /// Runtime-computed — stays an Unsupported finding.
    Dynamic,
}

/// Where a scan's `inline-eval` classification comes from (#214, #243).
#[derive(Clone, Copy)]
pub enum EvalProofSource<'a> {
    /// Before recovery: nothing has parsed the sources yet, so every textual
    /// `eval(`/`new Function(` hit is reported as Unsupported *pending* the
    /// adapter's AST proof — never closed and never silently dropped. The
    /// AST proof is a full TS parse, too slow to run twice on first contact
    /// (#243), so recovery supplies it instead.
    PendingRecovery,
    /// The adapter's claims from an actual extraction: proven sites close,
    /// const-shaped-but-unproven ones downgrade to potential Gaps, and the
    /// rest stay Unsupported (AC-0099).
    Claims(&'a [EvalSiteCoverage]),
    /// [`Self::Claims`] read against the exact bytes the extraction proved
    /// them on (#439 review): a JS/TS file the lookup returns is scanned
    /// from those bytes instead of the live tree, so an edit made after
    /// capture can never inherit a stale claim at the same path and line.
    /// A file the lookup lacks was never extracted, has no claims, and is
    /// read live — its eval lines stay Unsupported. A member deleted or
    /// renamed since capture is still scanned from its bytes, so its claims
    /// are never silently dropped (#439 review).
    Captured {
        /// The capture's members, in manifest order.
        paths: &'a [&'a str],
        /// The adapter's claims from extracting `bytes`.
        sites: &'a [EvalSiteCoverage],
        /// Retained source bytes by repo-relative path.
        bytes: &'a dyn Fn(&str) -> Option<&'a [u8]>,
    },
}

impl std::fmt::Debug for EvalProofSource<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PendingRecovery => f.write_str("PendingRecovery"),
            Self::Claims(sites) => f.debug_tuple("Claims").field(sites).finish(),
            Self::Captured { paths, sites, .. } => f
                .debug_struct("Captured")
                .field("paths", paths)
                .field("sites", sites)
                .finish_non_exhaustive(),
        }
    }
}

impl<'a> EvalProofSource<'a> {
    fn claims(self) -> Option<&'a [EvalSiteCoverage]> {
        match self {
            Self::PendingRecovery => None,
            Self::Claims(sites) | Self::Captured { sites, .. } => Some(sites),
        }
    }
}

/// One step of [`preflight_scan`]: a directory about to be listed while the
/// walk enumerates the tree, then each file about to be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanStep<'a> {
    /// Repo-relative path of the file about to be read — or, while listing
    /// (`total` is `None`), of the directory about to be entered (empty for
    /// the root).
    pub path: &'a str,
    /// Files already read before this one — or, while listing, files found
    /// so far.
    pub done: usize,
    /// Files in the whole walk; `None` while the walk is still listing the
    /// tree and the total is not yet known (#453).
    pub total: Option<usize>,
}

/// Run the preflight scan over `root`. Purely local; deterministic for a
/// given tree (files walked in sorted order).
pub fn preflight(root: &Path) -> std::io::Result<PreflightReport> {
    preflight_with_plugins(root, &[])
}

/// Preflight with gated plugin adapters in the coverage set (#201): an
/// extension a plugin claims counts as covered (the plugin id is the
/// adapter), so the uncovered-language finding for it closes on the next
/// scan instead of dead-ending. Compiled-in adapters win over plugins.
pub fn preflight_with_plugins(
    root: &Path,
    plugins: &[PluginCoverage],
) -> std::io::Result<PreflightReport> {
    preflight_with_coverage(root, plugins, &[])
}

/// [`preflight_with_plugins`] plus the adapter's eval-site claims (#214):
/// an `eval(` or `new Function(` line whose every AST site is a proven
/// literal is covered — no finding, it closes on this scan like plugin
/// coverage closes uncovered-language findings — while const-shaped-but-
/// unproven sites downgrade to explicit potential Gaps and everything else
/// (including lines the adapter never classified) stays Unsupported.
/// Never a guess.
pub fn preflight_with_coverage(
    root: &Path,
    plugins: &[PluginCoverage],
    eval_sites: &[EvalSiteCoverage],
) -> std::io::Result<PreflightReport> {
    Ok(preflight_scan(
        root,
        plugins,
        EvalProofSource::Claims(eval_sites),
        &mut |_| std::ops::ControlFlow::Continue(()),
    )?
    .expect("a callback that never breaks runs to completion"))
}

/// The preflight scan with an explicit eval-proof source and a per-file hook
/// that runs before each file is read, so a host can report live progress
/// and stop early (#235). Returns `Ok(None)` when the hook breaks — never a
/// partial report, because a missing finding would read as a clean tree.
pub fn preflight_scan(
    root: &Path,
    plugins: &[PluginCoverage],
    eval: EvalProofSource<'_>,
    on_file: &mut dyn FnMut(ScanStep<'_>) -> std::ops::ControlFlow<()>,
) -> std::io::Result<Option<PreflightReport>> {
    let mut languages: BTreeMap<String, LanguageDetection> = BTreeMap::new();
    let mut frameworks: Vec<String> = Vec::new();
    let mut unsupported = Vec::new();
    let mut potential_gaps = Vec::new();

    // Collect first so progress can name the walk's total. The walk is the
    // one every extractor uses (#248): `.gitignore`-aware and sorted, so
    // Preflight never reports files recovery would not read. The hook runs
    // before each directory too, so listing a large tree reports coarse
    // progress and a cancel stops it mid-walk (#453).
    let Some(walked) = source_walk::files_until(
        root,
        &skip_dir,
        source_walk::Gitignores::Honor,
        &mut |step| {
            on_file(ScanStep {
                path: step.dir,
                done: step.found,
                total: None,
            })
        },
    )?
    else {
        return Ok(None);
    };
    let files: Vec<_> = walked
        .into_iter()
        .map(|file| (file.path, file.name, file.rel))
        .collect();

    let total = files.len();
    let mut walked = std::collections::BTreeSet::new();
    for (done, (path, name, rel)) in files.into_iter().enumerate() {
        if on_file(ScanStep {
            path: &rel,
            done,
            total: Some(total),
        })
        .is_break()
        {
            return Ok(None);
        }
        // Framework chips and config-behind-code findings quote the
        // toolchain registry (#215) — the same detection that produces
        // the graph's Tool facts, so the two surfaces cannot disagree.
        let detection = crate::toolchain::detect_in_file(&name, &rel, &path);
        if detection
            .framework_labels
            .iter()
            .any(|label| label == crate::toolchain::WEBEXTENSION_LABEL)
        {
            // The WebExtension adapter claims manifests, not an extension,
            // so it is detected here and carries its matrix cells (AC-0253).
            detect_manifest_adapter("WebExtension", &mut languages);
        }
        frameworks.extend(detection.framework_labels);
        for tool in &detection.tools {
            if tool.settings_behind_code {
                unsupported.push(PatternFinding {
                    kind: "config-behind-code".into(),
                    path: rel.clone(),
                    line: 1,
                    message: format!(
                        "{} is authored in code — detected by presence; its \
                             settings are not evaluated and stay uncited",
                        tool.display
                    ),
                    detector: DETECTOR_ID.into(),
                    request_adapter: None,
                });
            }
        }
        // Pipeline limits (#1790, AC-0598): another CI system's configuration
        // is present and not read; a workflow or Dockerfile that cannot be
        // read exactly yields no step facts. Both are named, never skipped.
        if let Some(limit) = crate::pipelines::unread(&name, &rel, &path) {
            unsupported.push(finding(limit.kind, &rel, 1, &limit.message));
        }
        // Environment limits (#1791, AC-0661): a Kubernetes manifest, a
        // kustomize file and a Helm chart or values file are present and not
        // read. Compose files are read and carry no limit here.
        if let Some(limit) = crate::environments::unread(&name, &rel, &path) {
            unsupported.push(finding(limit.kind, &rel, 1, &limit.message));
        }
        let Some(extension) = path.extension().map(|e| e.to_string_lossy().into_owned()) else {
            continue;
        };
        // Risky-construct scanning is per-syntax, not per-coverage: a
        // .ts file gets the same eval/WASM findings as .js (#192
        // review) — both extensions are covered by the same adapter.
        if is_script(&extension) {
            scan_source(&path, &rel, eval, &mut unsupported, &mut potential_gaps)?;
            walked.insert(rel.clone());
        }
        detect_language(&extension, rel, plugins, &mut languages, &mut unsupported);
    }
    // Go recovery walks its own scope (it reads `target/`, for one), so the
    // skips come from that walk rather than this one (#1258), including in
    // a tree whose only Go is somewhere this walk skips. It stays under the
    // same hook: each step reports the finished file count, and a break
    // stops the scan.
    {
        let mut on_step = |path: &str| {
            on_file(ScanStep {
                path,
                done: total,
                total: Some(total),
            })
        };
        let Some(skipped) = adapters_lang_go::skipped_files_until(root, &mut on_step)
            .map_err(std::io::Error::other)?
        else {
            return Ok(None);
        };
        go_skipped_sources(&skipped, &mut unsupported);
    }

    // Captured members the live walk no longer reaches still carry the
    // extraction's claims; scan their captured bytes. No hook: the host's
    // reconciliation runs these to completion.
    if let EvalProofSource::Captured { paths, .. } = eval {
        for rel in paths.iter().copied().filter(|rel| !walked.contains(*rel)) {
            let rel_path = Path::new(rel);
            let skipped = rel_path
                .parent()
                .into_iter()
                .flat_map(Path::components)
                .any(|part| skip_dir(&part.as_os_str().to_string_lossy()));
            let script = rel_path
                .extension()
                .is_some_and(|ext| is_script(&ext.to_string_lossy()));
            if script && !skipped {
                scan_source(
                    &root.join(rel),
                    rel,
                    eval,
                    &mut unsupported,
                    &mut potential_gaps,
                )?;
                // Recovery published this file's facts, so it counts as a
                // detected source exactly as a walked one would.
                let extension = rel_path.extension().unwrap_or_default().to_string_lossy();
                detect_language(
                    &extension,
                    rel.to_string(),
                    plugins,
                    &mut languages,
                    &mut unsupported,
                );
            }
        }
    }

    frameworks.sort();
    frameworks.dedup();
    let languages: Vec<LanguageDetection> = languages.into_values().collect();
    let declared: Vec<PluginDeclaration> = plugins.iter().map(PluginDeclaration::from).collect();
    let adapter_lookup = report_lookup(&languages, &declared);
    Ok(Some(PreflightReport {
        languages,
        adapter_lookup,
        frameworks,
        unsupported,
        potential_gaps,
        detector: DETECTOR_ID.into(),
    }))
}

/// Count one manifest for a compiled-in adapter that claims manifests rather
/// than source extensions (WebExtension).
fn detect_manifest_adapter(language: &str, languages: &mut BTreeMap<String, LanguageDetection>) {
    let Some(adapter) = INSTALLED_ADAPTERS.iter().find(|a| a.language == language) else {
        return;
    };
    languages
        .entry(language.to_string())
        .or_insert(LanguageDetection {
            language: language.to_string(),
            files: 0,
            adapter: Some(adapter.id.to_string()),
            coverage: cells_for(language).map(ReportedCell::from).collect(),
        })
        .files += 1;
}

/// Counts one source file toward its language — covered by a built-in
/// adapter, by a gated plugin, or named-only with an `uncovered-language`
/// finding on its first file.
fn detect_language(
    extension: &str,
    rel: String,
    plugins: &[PluginCoverage],
    languages: &mut BTreeMap<String, LanguageDetection>,
    unsupported: &mut Vec<PatternFinding>,
) {
    let plugin_claim = plugins
        .iter()
        .find(|plugin| plugin.extensions.iter().any(|e| e == extension));
    if let Some((language, adapter)) = adapter_for(extension) {
        let entry = languages
            .entry(language.to_string())
            .or_insert(LanguageDetection {
                language: language.to_string(),
                files: 0,
                adapter: Some(adapter.to_string()),
                coverage: cells_for(language).map(ReportedCell::from).collect(),
            });
        entry.files += 1;
    } else if let Some(plugin) = plugin_claim {
        // A gated plugin covers this extension (#201): report it as
        // covered under the plugin id — no unsupported finding.
        // A new language the core cannot name takes the name the corpus
        // gives its extension, else the bare extension.
        let language = uncovered_language(extension)
            .map(str::to_string)
            .or_else(|| plugin.languages.get(extension).cloned())
            .unwrap_or_else(|| format!(".{extension}"));
        // Only this language's cells: one plugin may declare several
        // languages with differing support.
        let coverage = plugin
            .cells
            .iter()
            .filter(|cell| cell.language.eq_ignore_ascii_case(&language))
            .cloned()
            .collect();
        let entry = languages
            .entry(language.clone())
            .or_insert(LanguageDetection {
                language,
                files: 0,
                adapter: Some(plugin.plugin_id.clone()),
                coverage,
            });
        entry.files += 1;
    } else if let Some(language) = uncovered_language(extension) {
        let entry = languages
            .entry(language.to_string())
            .or_insert(LanguageDetection {
                language: language.to_string(),
                files: 0,
                adapter: None,
                coverage: Vec::new(),
            });
        entry.files += 1;
        if entry.files == 1 {
            // A planned adapter type is a recommendation, not a dead
            // end (#163): name the missing adapter and where to ask.
            let message = if planned_adapter_for(language) {
                format!(
                    "{language} sources present but no adapter covers them — \
                         a tool limitation, not a System Gap. A {language} adapter \
                         is a known adapter type: request it from Settings → Adapters"
                )
            } else {
                format!(
                    "{language} sources present but no adapter covers them — \
                         a tool limitation, not a System Gap"
                )
            };
            unsupported.push(PatternFinding {
                kind: "uncovered-language".into(),
                path: rel,
                line: 1,
                message,
                detector: DETECTOR_ID.into(),
                request_adapter: Some(language.to_string()),
            });
        }
    }
}

fn is_script(extension: &str) -> bool {
    matches!(
        extension,
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "vue"
    )
}

/// Line-level construct detectors for covered JS/TS sources and Vue script
/// blocks. Each detector
/// states which lane it feeds: unsupported (tool limitation) or potential
/// gap (statically unresolvable evidence).
fn scan_source(
    path: &Path,
    rel: &str,
    eval: EvalProofSource<'_>,
    unsupported: &mut Vec<PatternFinding>,
    potential_gaps: &mut Vec<PatternFinding>,
) -> std::io::Result<()> {
    let captured = match eval {
        EvalProofSource::Captured { bytes, .. } => bytes(rel),
        _ => None,
    };
    let text = match captured {
        Some(bytes) => std::str::from_utf8(bytes)
            .ok()
            .map(std::borrow::Cow::Borrowed),
        None => std::fs::read_to_string(path)
            .ok()
            .map(std::borrow::Cow::Owned),
    };
    let Some(text) = text else {
        return Ok(()); // non-UTF-8 source: nothing to scan
    };
    // A Vue component is scanned over the extractor's own script view
    // (AC-0268): template and style text never raises a finding, and line
    // numbers stay the component's.
    let text = if adapters_fw::vue::is_component(rel) {
        match String::from_utf8(adapters_fw::vue::script_view(text.as_bytes())) {
            Ok(view) => std::borrow::Cow::Owned(view),
            Err(_) => return Ok(()),
        }
    } else {
        text
    };
    for (index, line) in text.lines().enumerate() {
        let line_no = (index + 1) as u64;
        let has_eval = line.contains("eval(");
        let has_new_function = line.contains("new Function(");
        if (has_eval || has_new_function) && matches!(eval, EvalProofSource::PendingRecovery) {
            let message = if has_eval {
                "inline eval() — pending AST proof at recovery: closes if its \
                 argument proves to a literal, becomes a potential Gap if \
                 const-shaped but unproven, otherwise stays Unsupported"
            } else {
                "new Function() — pending AST proof at recovery: closes if its \
                 body proves to a literal, becomes a potential Gap if \
                 const-shaped but unproven, otherwise stays Unsupported"
            };
            unsupported.push(finding("inline-eval", rel, line_no, message));
        } else if let (true, Some(eval_sites)) = (has_eval || has_new_function, eval.claims()) {
            // The adapter's AST claims refine the textual hit (#214); both
            // dynamic-code constructs share one claim pool per line,
            // worst-wins: one unproven site keeps the line flagged even
            // next to a proven one — never guess. A line with no claim at
            // all (nothing the adapter recognized as the global
            // eval/Function) keeps its Unsupported finding.
            let claims: Vec<EvalProof> = eval_sites
                .iter()
                .filter(|claim| claim.path == rel && claim.line == line_no)
                .map(|claim| claim.proof)
                .collect();
            if claims.is_empty() || claims.contains(&EvalProof::Dynamic) {
                let message = if has_eval {
                    "inline eval() — no adapter can extract facts from \
                     dynamically evaluated code"
                } else {
                    "new Function() — no adapter can extract facts from \
                     dynamically constructed code"
                };
                unsupported.push(finding("inline-eval", rel, line_no, message));
            } else if claims.contains(&EvalProof::ConstUnproven) {
                potential_gaps.push(finding(
                    "inline-eval",
                    rel,
                    line_no,
                    "a const-shaped eval()/new Function() argument the \
                     adapter could not prove to a literal — recovery records \
                     it as an explicit Gap owned by its enclosing symbol, or by \
                     its File at top level",
                ));
            }
            // Every site on the line proved literal: the adapter extracted
            // its facts as Confirmed T0 — covered, the finding closes.
        }
        if line.contains("WebAssembly.instantiate") {
            unsupported.push(finding(
                "wasm-module",
                rel,
                line_no,
                "WebAssembly module instantiation — no adapter covers WASM",
            ));
        }
        if line.contains("executeScript") {
            potential_gaps.push(finding(
                "dynamic-injection",
                rel,
                line_no,
                "dynamically injected script body — becomes an explicit Gap \
                 if recovery cannot resolve the injected function statically",
            ));
        }
        // `import(` followed by anything but a string literal is a
        // runtime-computed module identity.
        if let Some(offset) = line.find("import(") {
            let after = line[offset + "import(".len()..].trim_start();
            if !after.starts_with('\'') && !after.starts_with('"') && !after.starts_with('`') {
                potential_gaps.push(finding(
                    "computed-import",
                    rel,
                    line_no,
                    "dynamic import with a runtime-computed specifier — \
                     becomes an explicit Gap if unresolvable",
                ));
            }
        }
    }
    Ok(())
}

/// One `go-skipped-source` finding per reason Go recovery skips files
/// (#1258): the count and the first path in recovery's walk order.
fn go_skipped_sources(
    skipped: &[(String, adapters_lang_go::SkipReason)],
    unsupported: &mut Vec<PatternFinding>,
) {
    use adapters_lang_go::SkipReason;
    for (reason, label) in [
        (SkipReason::PlatformSuffix, "a GOOS/GOARCH file-name suffix"),
        (
            SkipReason::BuildConstraint,
            "a //go:build or // +build constraint",
        ),
    ] {
        let mut paths = skipped
            .iter()
            .filter(|(_, why)| *why == reason)
            .map(|(rel, _)| rel);
        let Some(first) = paths.next() else { continue };
        let count = 1 + paths.count();
        let message = format!(
            "{count} Go source file(s) carry {label} and are not analysed: \
             recovery does not evaluate build tags (first: {first})"
        );
        unsupported.push(finding("go-skipped-source", first, 1, &message));
    }
}

fn finding(kind: &str, path: &str, line: u64, message: &str) -> PatternFinding {
    PatternFinding {
        kind: kind.into(),
        path: path.into(),
        line,
        message: message.into(),
        detector: DETECTOR_ID.into(),
        request_adapter: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn an_uncovered_language_looks_up_its_core_cells() {
        // AC-0361 (#1101): a language no adapter covers reports no matrix
        // cells, so the lookup names the core cells a first adapter claims.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app.rb", "class App\nend\n");
        let report = preflight(dir.path()).unwrap();
        let ruby: Vec<_> = report
            .adapter_lookup
            .iter()
            .filter(|lookup| lookup.cell.language == "Ruby")
            .map(|lookup| {
                (
                    lookup.cell.framework.as_str(),
                    lookup.cell.capability.as_str(),
                    &lookup.status,
                )
            })
            .collect();
        let none = crate::adapter_lookup::LookupStatus::None;
        assert_eq!(
            ruby,
            [
                ("core", "calls", &none),
                ("core", "imports", &none),
                ("core", "types", &none)
            ]
        );
    }

    #[test]
    fn detected_languages_report_their_coverage_cells() {
        // AC-0253: preflight names supported, partial and unsupported cells.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "main.go", "package main\n");
        write(dir.path(), "lib.rs", "fn main() {}\n");
        let report = preflight(dir.path()).unwrap();
        let go = report
            .languages
            .iter()
            .find(|l| l.language == "Go")
            .unwrap();
        assert!(go.coverage.iter().all(|cell| cell.language == "Go"));
        assert!(
            go.coverage
                .iter()
                .any(|cell| (cell.framework.as_str(), cell.capability.as_str())
                    == ("gin", "http_endpoints")
                    && cell.support == crate::matrix::Support::Supported)
        );
        assert!(
            go.coverage
                .iter()
                .any(|cell| (cell.framework.as_str(), cell.capability.as_str())
                    == ("kafka-go", "event_sites")
                    && cell.support == crate::matrix::Support::Partial
                    && !cell.note.is_empty())
        );
        let rust = report
            .languages
            .iter()
            .find(|l| l.language == "Rust")
            .unwrap();
        assert_eq!(rust.adapter.as_deref(), Some("t0.adapter-rust"));
        assert!(rust.coverage.iter().all(|cell| cell.language == "Rust"));
        assert!(rust.coverage.iter().any(|cell| (
            cell.framework.as_str(),
            cell.capability.as_str()
        ) == ("core", "imports")
            && cell.support == crate::matrix::Support::Supported));
    }

    #[test]
    fn classifies_unsupported_and_potential_gaps_distinctly() {
        // AC-0059 groundwork: unsupported ≠ gap, from first contact.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/app.ts",
            "const r = eval(input);\nchrome.scripting.executeScript({ func });\n",
        );
        write(
            root,
            "src/load.ts",
            "const mod = await import(pluginPath);\n",
        );
        write(
            root,
            "src/ok.ts",
            "import x from './x';\nconst m = import('./static');\n",
        );

        let report = preflight(root).unwrap();
        let unsupported: Vec<_> = report.unsupported.iter().map(|f| f.kind.as_str()).collect();
        let gaps: Vec<_> = report
            .potential_gaps
            .iter()
            .map(|f| f.kind.as_str())
            .collect();
        assert_eq!(unsupported, vec!["inline-eval"]);
        assert_eq!(gaps, vec!["dynamic-injection", "computed-import"]);
        // Static import specifiers never flag.
        assert!(
            !report
                .potential_gaps
                .iter()
                .any(|f| f.path.ends_with("ok.ts"))
        );
        // Every finding carries the versioned detector.
        assert!(report.unsupported.iter().all(|f| f.detector == DETECTOR_ID));
    }

    #[test]
    fn adapter_eval_claims_reclassify_inline_eval_findings() {
        // AC-0099 (#214): mirrors gated_plugin_coverage — the TS adapter's
        // AST proof is the single authority on eval sites; preflight only
        // consumes host-filled claims, so scan and extraction never disagree.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/app.ts",
            concat!(
                "eval('function f() {}');\n",  // proven literal
                "eval(CODE);\n",               // const-shaped, unproven
                "eval(input + '()');\n",       // runtime-computed
                "eval('x()'); eval(dyn());\n", // proven + dynamic on one line
                "new Function(body);\n",       // const-shaped, unproven
                "new Function('return 1');\n", // proven literal body
            ),
        );

        // Without claims every textual hit — eval() and new Function()
        // alike (#217 review) — stays Unsupported.
        let before = preflight(root).unwrap();
        assert_eq!(
            before
                .unsupported
                .iter()
                .filter(|f| f.kind == "inline-eval")
                .count(),
            6
        );
        assert!(before.potential_gaps.is_empty());

        let claim = |line: u64, proof: EvalProof| EvalSiteCoverage {
            path: "src/app.ts".into(),
            line,
            proof,
        };
        let claims = [
            claim(1, EvalProof::Covered),
            claim(2, EvalProof::ConstUnproven),
            claim(3, EvalProof::Dynamic),
            claim(4, EvalProof::Covered),
            claim(4, EvalProof::Dynamic),
            claim(5, EvalProof::ConstUnproven),
            claim(6, EvalProof::Covered),
        ];
        let after = preflight_with_coverage(root, &[], &claims).unwrap();
        let unsupported: Vec<u64> = after
            .unsupported
            .iter()
            .filter(|f| f.kind == "inline-eval")
            .map(|f| f.line)
            .collect();
        // Lines 1 and 6 are covered and close; line 3 stays; line 4 keeps
        // its finding because one site on it is dynamic (worst-wins, no
        // guess).
        assert_eq!(unsupported, vec![3, 4]);
        let gaps: Vec<u64> = after
            .potential_gaps
            .iter()
            .filter(|f| f.kind == "inline-eval")
            .map(|f| f.line)
            .collect();
        // The eval const line and the new Function const line both
        // downgrade — new Function claims reconcile through the same pool.
        assert_eq!(gaps, vec![2, 5]);
        // The downgraded finding still carries the versioned detector.
        assert!(
            after
                .potential_gaps
                .iter()
                .all(|f| f.detector == DETECTOR_ID)
        );
    }

    #[test]
    fn captured_claims_reconcile_against_the_proven_bytes_not_the_live_tree() {
        // AC-0200 (#439 review): the claims were proven on the captured
        // bytes, and the register must describe that same snapshot. After
        // capture the live file loses its dynamic eval and gains a literal
        // one on a line the claims never saw.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/app.ts", "const a = 1;\neval('x()');\n");
        write(root, "src/new.ts", "eval(later());\n");
        let captured: &[u8] = b"eval(getCode());\neval(CODE);\n";
        let claim = |line: u64, proof: EvalProof| EvalSiteCoverage {
            path: "src/app.ts".into(),
            line,
            proof,
        };
        let claims = [
            claim(1, EvalProof::Dynamic),
            claim(2, EvalProof::ConstUnproven),
        ];
        let lines = |findings: &[PatternFinding]| -> Vec<(String, u64)> {
            findings
                .iter()
                .filter(|f| f.kind == "inline-eval")
                .map(|f| (f.path.clone(), f.line))
                .collect()
        };

        // Scanning the live tree applies the claims to bytes they were not
        // proven on: the recovered dynamic eval (line 1) vanishes from the
        // register, and the new literal inherits line 2's const claim.
        let live = preflight_with_coverage(root, &[], &claims).unwrap();
        assert_eq!(lines(&live.unsupported), vec![("src/new.ts".into(), 1)]);
        assert_eq!(lines(&live.potential_gaps), vec![("src/app.ts".into(), 2)]);

        let bytes = |rel: &str| (rel == "src/app.ts").then_some(captured);
        let captured_scan = || {
            preflight_scan(
                root,
                &[],
                EvalProofSource::Captured {
                    paths: &["src/app.ts"],
                    sites: &claims,
                    bytes: &bytes,
                },
                &mut |_| std::ops::ControlFlow::Continue(()),
            )
            .unwrap()
            .unwrap()
        };
        let report = captured_scan();
        // The captured snapshot: line 1 dynamic, line 2 a const-shaped Gap.
        // A file outside the capture has no claims and stays Unsupported.
        assert_eq!(
            lines(&report.unsupported),
            vec![("src/app.ts".into(), 1), ("src/new.ts".into(), 1)]
        );
        assert_eq!(
            lines(&report.potential_gaps),
            vec![("src/app.ts".into(), 2)]
        );

        // Deleted after capture: the live walk never reaches it, but the
        // recovered graph still holds its facts, so its claims still apply.
        std::fs::remove_file(root.join("src/app.ts")).unwrap();
        let deleted = captured_scan();
        assert_eq!(
            lines(&deleted.unsupported),
            vec![("src/new.ts".into(), 1), ("src/app.ts".into(), 1)]
        );
        assert_eq!(
            lines(&deleted.potential_gaps),
            vec![("src/app.ts".into(), 2)]
        );
        // …and it still counts as a detected source (#439 review): with
        // src/new.ts gone too, the report must not read "no sources".
        std::fs::remove_file(root.join("src/new.ts")).unwrap();
        let only_captured = captured_scan();
        let counts: Vec<_> = only_captured
            .languages
            .iter()
            .map(|l| (l.language.as_str(), l.files))
            .collect();
        assert_eq!(counts, vec![("TypeScript", 1)]);
    }

    #[test]
    fn detects_languages_frameworks_and_adapter_coverage() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/app.ts", "export const a = 1;\n");
        write(root, "main.go", "package main\n");
        write(
            root,
            "infra/net.tf",
            "resource \"aws_sqs_queue\" \"q\" {}\n",
        );
        write(root, "legacy/report.rb", "puts 'hi'\n");
        write(
            root,
            "manifest.json",
            "{\"manifest_version\": 3, \"name\": \"x\"}\n",
        );
        write(
            root,
            "package.json",
            "{\"dependencies\": {\"react\": \"^19\"}}\n",
        );
        // Ignored tree must not contribute detections.
        write(root, "node_modules/dep/index.js", "eval('x')\n");

        let report = preflight(root).unwrap();
        let by_language: BTreeMap<_, _> = report
            .languages
            .iter()
            .map(|l| (l.language.as_str(), l))
            .collect();
        assert_eq!(
            by_language["TypeScript"].adapter.as_deref(),
            Some("t0.adapter-ts")
        );
        assert_eq!(by_language["Go"].adapter.as_deref(), Some("t0.adapter-go"));
        assert_eq!(
            by_language["Terraform"].adapter.as_deref(),
            Some("t0.iac-terraform")
        );
        // Ruby is named but uncovered — and surfaces as an unsupported finding.
        assert_eq!(by_language["Ruby"].adapter, None);
        assert!(
            report
                .unsupported
                .iter()
                .any(|f| f.kind == "uncovered-language" && f.message.contains("Ruby"))
        );
        assert!(report.frameworks.iter().any(|f| f.contains("WebExtension")));
        // The manifest-claimed adapter carries its matrix cells too (AC-0253).
        let webextension = &by_language["WebExtension"];
        assert_eq!(webextension.adapter.as_deref(), Some("t0.webextension"));
        assert_eq!((webextension.files, webextension.coverage.len()), (1, 1));
        assert!(report.frameworks.contains(&"React".to_string()));
        // node_modules is skipped: no eval finding from the dependency.
        assert!(
            !report
                .unsupported
                .iter()
                .any(|f| f.path.contains("node_modules"))
        );
    }

    #[test]
    fn javascript_is_covered_not_unsupported() {
        // AC-0095: JavaScript used to be a PLANNED_ADAPTERS recommendation
        // ("request it from Settings → Adapters"); the TypeScript crate's
        // grammar now parses it directly, so a JS-only tree is covered like
        // any other installed adapter — no uncovered-language finding, no
        // adapter-request message.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/index.js", "export function run() {}\n");
        write(root, "src/Widget.jsx", "export function Widget() {}\n");

        let report = preflight(root).unwrap();
        let by_language: BTreeMap<_, _> = report
            .languages
            .iter()
            .map(|l| (l.language.as_str(), l))
            .collect();
        assert_eq!(
            by_language["JavaScript"].adapter.as_deref(),
            Some("t0.adapter-ts")
        );
        assert_eq!(by_language["JavaScript"].files, 2);
        assert!(
            !report
                .unsupported
                .iter()
                .any(|f| f.message.contains("JavaScript"))
        );
        assert!(!planned_adapter_for("JavaScript"));
    }

    #[test]
    fn vue_components_are_covered_for_script_content_only() {
        // AC-0268 (#595): `.vue` files are covered by the TypeScript crate
        // for their script blocks, and the report says templates are not.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "src/App.vue",
            "<template><p/></template>\n<script setup>\nconst a = 1\n</script>\n",
        );
        let report = preflight(dir.path()).unwrap();
        let vue = report
            .languages
            .iter()
            .find(|l| l.language == "Vue")
            .expect("Vue is detected");
        assert_eq!(vue.adapter.as_deref(), Some("t0.adapter-ts"));
        assert_eq!(vue.files, 1);
        assert!(
            vue.coverage
                .iter()
                .any(|cell| cell.note.contains("templates are not parsed"))
        );
        let adapter = INSTALLED_ADAPTERS
            .iter()
            .find(|a| a.language == "Vue")
            .unwrap();
        assert!(
            adapter
                .covers
                .contains("templates and styles are not parsed")
        );
        assert!(
            !report
                .unsupported
                .iter()
                .any(|f| f.kind == "uncovered-language")
        );
    }

    #[test]
    fn vue_script_blocks_get_risky_construct_findings_and_templates_do_not() {
        // AC-0268 (#595 review): risky constructs in a component's script
        // are reported at the component's own line; the same text in its
        // template or style is not script and raises nothing.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "src/Run.vue",
            "<template><p>{{ eval(x) }} WebAssembly.instantiate</p></template>\n\
             <script setup>\n\
             const m = WebAssembly.instantiate(bytes)\n\
             eval(code)\n\
             </script>\n\
             <style>.executeScript {}</style>\n",
        );
        let report = preflight(dir.path()).unwrap();
        let lines = |kind: &str| -> Vec<u64> {
            report
                .unsupported
                .iter()
                .chain(&report.potential_gaps)
                .filter(|f| f.kind == kind && f.path == "src/Run.vue")
                .map(|f| f.line)
                .collect()
        };
        assert_eq!(lines("wasm-module"), vec![3]);
        assert_eq!(lines("inline-eval"), vec![4]);
        assert!(lines("dynamic-injection").is_empty());
    }

    #[test]
    fn adapter_registry_drives_coverage_and_recommendations() {
        // AC-0087 (#163): one registry serves Preflight and Settings — every
        // installed adapter's extensions resolve back to exactly that
        // adapter, so the inventory can never disagree with coverage.
        for adapter in INSTALLED_ADAPTERS {
            for extension in adapter.extensions {
                assert_eq!(adapter_for(extension), Some((adapter.language, adapter.id)));
            }
        }
        // Planned adapter types are detectable and marked requestable.
        for planned in PLANNED_ADAPTERS {
            assert!(planned_adapter_for(planned.language));
            for extension in planned.extensions {
                assert_eq!(uncovered_language(extension), Some(planned.language));
                assert_eq!(adapter_for(extension), None);
            }
        }
        // Named-only languages are honest but not in the request catalog.
        assert_eq!(uncovered_language("rb"), Some("Ruby"));
        assert!(!planned_adapter_for("Ruby"));

        // A detected planned language recommends the adapter in place.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "App.swift", "func main() {}\n");
        let report = preflight(dir.path()).unwrap();
        let finding = report
            .unsupported
            .iter()
            .find(|f| f.kind == "uncovered-language" && f.message.contains("Swift"))
            .expect("Swift surfaces as uncovered");
        assert!(
            finding
                .message
                .contains("request it from Settings → Adapters")
        );
    }

    #[test]
    fn kotlin_is_covered_not_unsupported() {
        // AC-0098: Kotlin used to be a PLANNED_ADAPTERS recommendation
        // ("request it from Settings → Adapters"); the compiled-in
        // adapters-lang-kotlin crate now covers `.kt`/`.kts`, so a Kotlin
        // tree is covered like any other installed adapter — no
        // uncovered-language finding, no adapter-request message.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/App.kt", "fun main() {}\n");
        write(root, "build.gradle.kts", "plugins { }\n");

        let report = preflight(root).unwrap();
        let by_language: BTreeMap<_, _> = report
            .languages
            .iter()
            .map(|l| (l.language.as_str(), l))
            .collect();
        assert_eq!(
            by_language["Kotlin"].adapter.as_deref(),
            Some("t0.adapter-kotlin")
        );
        assert_eq!(by_language["Kotlin"].files, 2);
        assert!(
            !report
                .unsupported
                .iter()
                .any(|f| f.message.contains("Kotlin"))
        );
        assert!(!planned_adapter_for("Kotlin"));
    }

    #[test]
    fn gated_plugin_coverage_closes_the_uncovered_finding() {
        // AC-0093 (#201): the same tree, rescanned with a gated plugin
        // claiming the extension, reports the language covered under the
        // plugin id — the uncovered-language finding closes — and the
        // finding it replaces carried the request-adapter action.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app.rb", "puts 'hi'\n");

        let before = preflight(dir.path()).unwrap();
        let finding = before
            .unsupported
            .iter()
            .find(|f| f.kind == "uncovered-language" && f.message.contains("Ruby"))
            .expect("Ruby surfaces as uncovered");
        assert_eq!(finding.request_adapter.as_deref(), Some("Ruby"));

        let cell = ReportedCell {
            language: "Ruby".into(),
            framework: "rails".into(),
            capability: "http_endpoints".into(),
            support: crate::matrix::Support::Supported,
            note: String::new(),
        };
        let plugins = [PluginCoverage {
            plugin_id: "t0.plugin-ruby".into(),
            version: "hash".into(),
            extensions: vec!["rb".into()],
            cells: vec![cell.clone()],
            languages: Default::default(),
        }];
        let after = preflight_with_plugins(dir.path(), &plugins).unwrap();
        let ruby = after
            .languages
            .iter()
            .find(|l| l.language == "Ruby")
            .expect("Ruby still detected");
        assert_eq!(ruby.adapter.as_deref(), Some("t0.plugin-ruby"));
        // The gated plugin's declared cells are reported with it (AC-0263).
        assert_eq!(ruby.coverage, vec![cell]);
        assert!(
            !after
                .unsupported
                .iter()
                .any(|f| f.kind == "uncovered-language" && f.message.contains("Ruby"))
        );

        // Compiled-in adapters always win over a plugin claiming the same
        // extension — first-class languages stay compiled in.
        write(dir.path(), "main.ts", "export const x = 1;\n");
        let contested = [PluginCoverage {
            plugin_id: "t0.plugin-rogue".into(),
            version: "hash".into(),
            extensions: vec!["ts".into()],
            cells: vec![],
            languages: Default::default(),
        }];
        let report = preflight_with_plugins(dir.path(), &contested).unwrap();
        let ts = report
            .languages
            .iter()
            .find(|l| l.language == "TypeScript")
            .expect("TypeScript detected");
        assert_eq!(ts.adapter.as_deref(), Some("t0.adapter-ts"));
    }

    #[test]
    fn preflight_exposes_the_adapter_lookup_for_each_gap_cell() {
        // #1071: every non-supported cell carries who declares coverage.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "import os\n").unwrap();
        let report = preflight(dir.path()).unwrap();
        let gaps: Vec<_> = report
            .languages
            .iter()
            .flat_map(|l| &l.coverage)
            .filter(|c| c.support != crate::matrix::Support::Supported)
            .collect();
        assert!(!gaps.is_empty());
        assert_eq!(report.adapter_lookup.len(), gaps.len());

        // A declared-but-ungated plugin shows as unverified on refresh.
        let mut report = report;
        let target = report.adapter_lookup[0].cell.clone();
        let declared = [PluginDeclaration {
            id: "t0.plugin-x".into(),
            version: "h".into(),
            gate_passed: false,
            cells: vec![ReportedCell {
                support: crate::matrix::Support::Supported,
                note: String::new(),
                ..target.clone()
            }],
        }];
        report.refresh_adapter_lookup(&declared);
        assert!(matches!(
            report.adapter_lookup[0].status,
            crate::adapter_lookup::LookupStatus::DeclaredUnverified { .. }
        ));
    }

    #[test]
    fn a_multi_language_plugin_reports_each_language_only_its_own_cells() {
        // AC-0263: one plugin declaring the same capability for two
        // languages at differing support reports each language its own
        // cells, never the other language's claim.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app.rb", "puts 'hi'\n");
        write(dir.path(), "index.php", "<?php echo 'hi';\n");
        let cell = |language: &str, support, note: &str| ReportedCell {
            language: language.into(),
            framework: "core".into(),
            capability: "http_endpoints".into(),
            support,
            note: note.into(),
        };
        let ruby_cell = cell("ruby", crate::matrix::Support::Supported, "");
        let php_cell = cell(
            "PHP",
            crate::matrix::Support::Partial,
            "attribute routes only",
        );
        let plugins = [PluginCoverage {
            plugin_id: "t0.plugin-scripts".into(),
            version: "hash".into(),
            extensions: vec!["rb".into(), "php".into()],
            cells: vec![ruby_cell.clone(), php_cell.clone()],
            languages: Default::default(),
        }];
        let report = preflight_with_plugins(dir.path(), &plugins).unwrap();
        let coverage = |language: &str| {
            report
                .languages
                .iter()
                .find(|l| l.language == language)
                .map(|l| l.coverage.clone())
                .expect("language detected")
        };
        assert_eq!(coverage("Ruby"), vec![ruby_cell.clone()]);
        assert_eq!(coverage("PHP"), vec![php_cell]);

        // New languages the core cannot name take the corpus's names for
        // their extensions, so two of them never share each other's cells.
        write(dir.path(), "main.foo", "say hi\n");
        write(dir.path(), "main.bar", "say hi\n");
        let foo_cell = cell("Foo", crate::matrix::Support::Supported, "");
        let bar_cell = cell("Bar", crate::matrix::Support::Partial, "no routes");
        let plugins = [PluginCoverage {
            plugin_id: "t0.plugin-foo".into(),
            version: "hash".into(),
            extensions: vec!["foo".into(), "bar".into(), "rb".into()],
            cells: vec![foo_cell.clone(), bar_cell.clone(), ruby_cell.clone()],
            languages: [("foo", "Foo"), ("bar", "Bar")]
                .map(|(e, l)| (e.to_string(), l.to_string()))
                .into(),
        }];
        let report = preflight_with_plugins(dir.path(), &plugins).unwrap();
        let coverage = |language: &str| {
            report
                .languages
                .iter()
                .find(|l| l.language == language)
                .map(|l| l.coverage.clone())
                .expect("language detected")
        };
        assert_eq!(coverage("Foo"), vec![foo_cell]);
        assert_eq!(coverage("Bar"), vec![bar_cell]);
        assert_eq!(coverage("Ruby"), vec![ruby_cell]);
    }

    #[test]
    fn eval_sites_stay_pending_until_recovery_supplies_the_proof() {
        // AC-0199 (#243): before recovery no AST proof exists, so every
        // textual eval()/new Function() line is reported Unsupported and
        // marked pending — a proven literal is not claimed closed, and no
        // line is dropped.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/app.ts",
            concat!(
                "eval('function f() {}');\n",
                "eval(CODE);\n",
                "new Function('return 1');\n",
            ),
        );
        let report = preflight_scan(root, &[], EvalProofSource::PendingRecovery, &mut |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap()
        .expect("an uninterrupted scan completes");
        let pending: Vec<u64> = report
            .unsupported
            .iter()
            .filter(|f| f.kind == "inline-eval")
            .inspect(|f| assert!(f.message.contains("pending AST proof at recovery")))
            .map(|f| f.line)
            .collect();
        assert_eq!(pending, vec![1, 2, 3]);
        assert!(
            report
                .potential_gaps
                .iter()
                .all(|f| f.kind != "inline-eval")
        );
    }

    #[test]
    fn gitignored_trees_are_neither_scanned_nor_counted() {
        // AC-0205 (#248): Preflight walks exactly what recovery reads — a
        // `.gitignore`d `out/` bundle is not a detected source and its eval
        // lines are not findings; the non-ignored file still is.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, ".gitignore", "out/\n");
        write(root, "src/app.ts", "eval(code);\n");
        write(root, "out/bundle.js", "eval(code);\n");
        let report = preflight(root).unwrap();
        let files: u64 = report.languages.iter().map(|l| l.files).sum();
        assert_eq!(files, 1);
        let paths: Vec<_> = report.unsupported.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"src/app.ts"));
        assert!(!paths.iter().any(|path| path.starts_with("out/")));
    }

    #[test]
    fn scan_hook_reports_each_file_and_stops_without_a_partial_report() {
        // AC-0197/AC-0198 (#235): the walk announces each file with its
        // position before reading it, so a cancel stops it within one file,
        // and a break returns no report.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "a.ts", "export const a = 1;\n");
        write(root, "b.ts", "export const b = 1;\n");
        let mut seen = Vec::new();
        preflight_scan(root, &[], EvalProofSource::PendingRecovery, &mut |step| {
            seen.push((step.path.to_string(), step.done, step.total));
            std::ops::ControlFlow::Continue(())
        })
        .unwrap()
        .expect("completes");
        assert_eq!(
            seen,
            vec![
                (String::new(), 0, None),
                ("a.ts".to_string(), 0, Some(2)),
                ("b.ts".to_string(), 1, Some(2)),
                // The Go skip pass lists the tree again (#1258).
                (String::new(), 2, Some(2))
            ]
        );
        let mut announced = 0;
        let stopped = preflight_scan(root, &[], EvalProofSource::PendingRecovery, &mut |step| {
            if step.total.is_none() {
                return std::ops::ControlFlow::Continue(());
            }
            announced += 1;
            std::ops::ControlFlow::Break(())
        })
        .unwrap();
        assert!(stopped.is_none());
        assert_eq!(announced, 1);
    }

    #[test]
    fn scan_hook_reports_the_listing_walk_and_stops_it_mid_tree() {
        // AC-0197/AC-0198 (#453): before any file is read, the walk that
        // lists the tree announces each directory with the files found so
        // far, and a break there stops the listing itself — no later
        // directory is entered, no file is announced, and no report returns.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for sub in ["a", "b", "c"] {
            write(root, &format!("{sub}/x.ts"), "export const x = 1;\n");
            write(root, &format!("{sub}/notes.md"), "notes\n");
        }
        let mut seen = Vec::new();
        let stopped = preflight_scan(root, &[], EvalProofSource::PendingRecovery, &mut |step| {
            seen.push((step.path.to_string(), step.done, step.total));
            if step.path == "b" {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        })
        .unwrap();
        assert!(stopped.is_none());
        assert_eq!(
            seen,
            vec![
                (String::new(), 0, None),
                ("a".to_string(), 0, None),
                ("b".to_string(), 2, None)
            ]
        );
    }

    // AC-0426 (#1251): common languages with no adapter are named with their
    // file count and one uncovered-language finding, never silently skipped.
    #[test]
    fn languages_without_an_adapter_are_named_not_omitted() {
        let dir = tempfile::tempdir().unwrap();
        let named = [
            ("core/Broker.scala", "Scala"),
            ("src/Tasks.groovy", "Groovy"),
            ("app/main.dart", "Dart"),
            ("test/app_test.exs", "Elixir"),
            ("src/app.erl", "Erlang"),
            ("src/app.clj", "Clojure"),
            ("src/app.cljs", "Clojure"),
            ("src/app.cljc", "Clojure"),
            ("scripts/build.fsx", "F#"),
            ("src/Module.vb", "Visual Basic"),
            ("src/Main.hs", "Haskell"),
            ("scripts/init.lua", "Lua"),
        ];
        for (path, _) in named {
            write(dir.path(), path, "x\n");
        }
        // Ambiguous extensions stay unnamed rather than misnamed.
        for path in ["lib/app.ex", "shaders/frag.fs", "tools/run.pl"] {
            write(dir.path(), path, "x\n");
        }
        let report = preflight(dir.path()).unwrap();
        let mut expected: BTreeMap<&str, u64> = BTreeMap::new();
        for (_, language) in named {
            *expected.entry(language).or_default() += 1;
        }
        let found: BTreeMap<&str, u64> = report
            .languages
            .iter()
            .map(|language| {
                assert_eq!(language.adapter, None, "{}", language.language);
                (language.language.as_str(), language.files)
            })
            .collect();
        assert_eq!(found, expected);
        for language in expected.keys() {
            assert!(!planned_adapter_for(language));
            assert_eq!(
                report
                    .unsupported
                    .iter()
                    .filter(|finding| finding.kind == "uncovered-language"
                        && finding.request_adapter.as_deref() == Some(*language))
                    .count(),
                1,
                "{language}"
            );
        }
    }

    // AC-0427 (#1258): Go files the adapter skips are counted per reason,
    // never silently dropped; vendored, test and plain files are not counted.
    #[test]
    fn go_files_recovery_skips_are_named_with_counts() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "go.mod", "module example.com/app\n");
        write(dir.path(), "cmd/main.go", "package main\n");
        write(dir.path(), "pkg/sys_linux.go", "package pkg\n");
        write(dir.path(), "pkg/sys_windows_amd64.go", "package pkg\n");
        write(
            dir.path(),
            "pkg/tagged.go",
            "//go:build linux\n\npackage pkg\n",
        );
        write(
            dir.path(),
            "pkg/legacy.go",
            "// +build ignore\n\npackage pkg\n",
        );
        write(
            dir.path(),
            "pkg/late.go",
            "package pkg\n\n//go:build linux\n",
        );
        write(dir.path(), "pkg/sys_linux_test.go", "package pkg\n");
        write(dir.path(), "vendor/dep/dep_linux.go", "package dep\n");
        write(
            dir.path(),
            "vendor/dep/tagged.go",
            "//go:build linux\npackage dep\n",
        );
        // Preflight's own walk skips `target/`; Go recovery reads it.
        write(dir.path(), "target/gen/zz_linux.go", "package gen\n");
        let report = preflight(dir.path()).unwrap();
        let skipped: Vec<(&str, &str)> = report
            .unsupported
            .iter()
            .filter(|finding| finding.kind == "go-skipped-source")
            .map(|finding| (finding.path.as_str(), finding.message.as_str()))
            .collect();
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        assert_eq!(skipped[0].0, "pkg/sys_linux.go");
        assert!(
            skipped[0]
                .1
                .starts_with("3 Go source file(s) carry a GOOS/GOARCH")
        );
        assert_eq!(skipped[1].0, "pkg/legacy.go");
        assert!(
            skipped[1]
                .1
                .starts_with("2 Go source file(s) carry a //go:build")
        );
        assert_eq!(
            report.unsupported,
            preflight(dir.path()).unwrap().unsupported
        );

        // The Go pass runs under the scan hook: a break there stops the
        // scan, so no report returns after cancellation.
        let mut go_steps = Vec::new();
        let stopped = preflight_scan(
            dir.path(),
            &[],
            EvalProofSource::PendingRecovery,
            &mut |step| {
                if step.path == "target/gen/zz_linux.go" {
                    go_steps.push(step.path.to_string());
                    return std::ops::ControlFlow::Break(());
                }
                std::ops::ControlFlow::Continue(())
            },
        )
        .unwrap();
        assert!(stopped.is_none());
        assert_eq!(go_steps, vec!["target/gen/zz_linux.go".to_string()]);
    }

    // AC-0427 (#1258): a tree whose only Go sits where preflight's own walk
    // does not go (`target/`) still has its skipped Go files named.
    #[test]
    fn go_skips_are_named_when_all_go_is_outside_the_preflight_walk() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "target/go.mod", "module example.com/gen\n");
        write(dir.path(), "target/gen/zz_linux.go", "package gen\n");
        let report = preflight(dir.path()).unwrap();
        let skipped: Vec<&str> = report
            .unsupported
            .iter()
            .filter(|finding| finding.kind == "go-skipped-source")
            .map(|finding| finding.path.as_str())
            .collect();
        assert_eq!(skipped, vec!["target/gen/zz_linux.go"]);
    }
}
