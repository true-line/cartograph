//! Producer-bound primary source receipts (SPEC-06).
//!
//! The captured factory invokes the actual parser on the retained byte slice.
//! Only its private direct-output sink admits facts; public receipt validation
//! checks content integrity and does not establish who produced supplied JSON.
//! Directory enrichment still reads live configuration and is outside closure;
//! a walk reports whether the configs it read are the capture's own bytes.

use super::{ExtractError, Extraction, IncrementalStats, SourceId};
use adapters_fw::events::Registries;
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, OpenOptions};
use core_graph::facts::FactStore;
use core_graph::packed::{PackedEdge, PackedNode};
use core_graph::source::FactKey;
use core_graph::{Edge, Node};
use source_capture::{Capture, CaptureFileRef, CapturedFile};
use source_walk::IgnoreRules;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

mod python;
pub use python::{
    ReceiptStage, admit_after, mint_go_file_receipts, mint_java_file_receipts,
    mint_kotlin_file_receipts, mint_python_file_receipts, mint_rust_file_receipts,
    stage_lane_receipts,
};
mod closure;
pub use closure::{
    CLOSURE_ID_PREFIX, ClosureGapReason, ClosureInput, ClosureParameters, ClosureRef,
    ExternalMedia, InputClosureV1, MAX_CLOSURE_BYTES, MAX_CLOSURE_INPUTS, is_closure_id,
};
mod observed;
pub use observed::mint_observed_receipts;
mod receipt;
pub use receipt::{
    Citable, Grammar, MAX_RECEIPT_BYTES, MAX_RECEIPT_OTHER_FILES, MAX_RECEIPT_RANGES, MintedHeader,
    MintedReceipt, RangeRole, Receipt, ReceiptClosure, ReceiptRange, ReceiptSourceSizes,
    TS_DIRECTORY_WALK_POLICY, TS_GRAMMAR_RULE, TS_WALK_POLICY, direct_lexical_closure,
    directory_walk_closure, fact_match_workspace, fact_match_workspace_bytes, is_input_source,
};

/// Maximum directory entries examined, including excluded entries.
pub const MAX_VISITED_ENTRIES: usize = 100_000;
/// Ceiling on the directory entries a project may let one selection examine
/// (#1653). The walk retains only selected paths, so entries cost time, not
/// memory.
pub const HARD_MAX_VISITED_ENTRIES: usize = 1_000_000;
/// Maximum unfollowed symlink paths one selection retains for reporting. It
/// stays at the historical entry bound when a project raises
/// [`SelectionLimits::max_visited_entries`], so a raised walk never retains
/// more symlink paths than the default walk could; one past it fails the
/// selection whole.
pub const MAX_UNFOLLOWED_LINKS: usize = MAX_VISITED_ENTRIES;
/// Maximum nested directory depth below the host-selected root.
pub const MAX_DIRECTORY_DEPTH: usize = 64;
/// Maximum receipt memory retained in one adapter result, before host
/// persistence, measured by [`MintedReceipt::retained_bytes`] plus each shared
/// file header once (#725).
pub const MAX_EXTRACTION_RECEIPT_BYTES: usize = 128 * 1024 * 1024;
/// Maximum receipts returned by one captured directory extraction.
pub const MAX_EXTRACTION_RECEIPTS: usize = 200_000;
/// Maximum declaration receipts minted for one file (#691). Bounds what one
/// parsed file and the ordered worker queue retain before directory admission.
pub const MAX_FILE_DECLARATION_RECEIPTS: usize = 1024;
/// Maximum declaration receipt memory minted for one file, measured as the
/// extraction budget is.
pub const MAX_FILE_DECLARATION_BYTES: usize = 2 * 1024 * 1024;

/// Fixed diagnostics never include parsed source or untrusted receipt values.
#[derive(Debug, thiserror::Error)]
pub enum CapturedError {
    /// Invalid metadata, unsupported syntax binding, or malformed receipt.
    #[error("invalid primary source input: {0}")]
    Invalid(&'static str),
    /// A declared hard bound was exceeded.
    #[error("primary source limit exceeded: {0}")]
    Limit(&'static str),
    /// Source enumeration failed.
    #[error("primary source enumeration failed: {0}")]
    Io(#[from] std::io::Error),
    /// The retained capture could not supply a selected file or range.
    #[error("primary source capture failed: {0}")]
    Capture(#[from] source_capture::CaptureError),
    /// Ordinary parser or directory enrichment failure.
    #[error("primary source extraction failed: {0}")]
    Extraction(#[from] ExtractError),
}

/// Private producer knowledge recorded at direct emission, never inferred from
/// arbitrary public Extraction metadata. Recursive eval receives no such sink.
#[derive(Default)]
pub(super) struct DirectFacts {
    pub(super) owners: BTreeMap<String, Node>,
    /// Declaration, Decorator and Signature facts (#693), never rule owners.
    pub(super) declarations: BTreeMap<String, Node>,
    /// Locals of oversized callables (#699), minted last: they fill only the
    /// budget the file's own declarations leave, and the rest are counted.
    pub(super) locals: BTreeMap<String, Node>,
    pub(super) rules: Vec<(Node, Edge)>,
    /// The file's own File node and its TestCase facts (AC-1019): supporting
    /// facts a gap task reads, kept only if completion leaves them unchanged.
    pub(super) supporting: BTreeMap<String, Node>,
}

impl DirectFacts {
    /// Name `commit` in every recorded fact's evidence, as
    /// [`super::retarget_commit`] does for the file's extraction.
    fn retarget(&mut self, commit: &str) {
        for node in self
            .owners
            .values_mut()
            .chain(self.declarations.values_mut())
            .chain(self.locals.values_mut())
            .chain(self.supporting.values_mut())
        {
            super::retarget_props_commit(&mut node.props, commit);
        }
        for (node, edge) in &mut self.rules {
            super::retarget_props_commit(&mut node.props, commit);
            super::retarget_props_commit(&mut edge.props, commit);
        }
    }
}

/// Producer contract an attested parse is bound to (ADR-0087). The crate
/// version names the binary's parser and grammar pins; the suffix changes
/// whenever what one file's parse records changes without a release.
const PRODUCER_CONTRACT: &str = concat!(
    "adapters-lang-ts@",
    env!("CARGO_PKG_VERSION"),
    "/captured-parse@1"
);

/// What one captured file's parse was made from (ADR-0087): the producer
/// contract, the registry set, the registered source and repository, and
/// the exact retained bytes by path, digest and length. A parse is reused
/// only for an equal attestation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Attestation {
    producer: &'static str,
    registries: String,
    source: String,
    repo: String,
    path: String,
    digest: String,
    byte_len: u64,
}

impl Attestation {
    fn of(file: &CapturedFile, repo: &str, registries: &str) -> Self {
        let reference = file.reference();
        Self {
            producer: PRODUCER_CONTRACT,
            registries: registries.to_owned(),
            source: reference.source_id.as_str().to_owned(),
            repo: repo.to_owned(),
            path: reference.path.clone(),
            digest: reference.digest.clone(),
            byte_len: reference.byte_len,
        }
    }
}

/// One file's parse as the producer made it, packed (#725). Only this
/// module builds one, from its own parse of a retained buffer; nothing
/// decodes one from outside.
#[derive(Debug, Clone)]
struct AttestedParse {
    attestation: Attestation,
    /// The commit the parse's evidence names.
    commit: String,
    /// The input closure the parse was made under (ADR-0072): reuse proves
    /// a closure only when it is this one.
    closure_id: String,
    /// The extraction with its facts moved to `facts`.
    extraction: Extraction,
    facts: FactStore,
    direct: PackedDirect,
}

/// [`DirectFacts`] packed in order, each map's keys beside its facts.
#[derive(Debug, Clone)]
struct PackedDirect {
    keys: [Vec<String>; 4],
    maps: [FactStore; 4],
    rules: FactStore,
}

impl AttestedParse {
    fn pack(
        attestation: Attestation,
        commit: &str,
        closure_id: String,
        extraction: &Extraction,
        direct: DirectFacts,
    ) -> Result<Self, CapturedError> {
        let mut shell = extraction.clone();
        let facts = FactStore::from_facts(
            std::mem::take(&mut shell.nodes),
            std::mem::take(&mut shell.edges),
        )
        .map_err(ExtractError::from)?;
        let pack = |map: BTreeMap<String, Node>| {
            let (keys, nodes): (Vec<String>, Vec<Node>) = map.into_iter().unzip();
            FactStore::from_facts(nodes, Vec::new())
                .map(|store| (keys, store))
                .map_err(ExtractError::from)
        };
        let (owners, owner_facts) = pack(direct.owners)?;
        let (declarations, declaration_facts) = pack(direct.declarations)?;
        let (locals, local_facts) = pack(direct.locals)?;
        let (supporting, supporting_facts) = pack(direct.supporting)?;
        let (rule_nodes, rule_edges) = direct.rules.into_iter().unzip();
        Ok(Self {
            attestation,
            commit: commit.to_owned(),
            closure_id,
            extraction: shell,
            facts,
            direct: PackedDirect {
                keys: [owners, declarations, locals, supporting],
                maps: [
                    owner_facts,
                    declaration_facts,
                    local_facts,
                    supporting_facts,
                ],
                rules: FactStore::from_facts(rule_nodes, rule_edges).map_err(ExtractError::from)?,
            },
        })
    }

    /// The parse decoded, its evidence naming `commit`.
    fn unpack(&self, commit: &str) -> Result<(Extraction, DirectFacts), CapturedError> {
        let mut extraction = self.extraction.clone();
        (extraction.nodes, extraction.edges) = self
            .facts
            .clone()
            .into_facts()
            .map_err(ExtractError::from)?;
        let mut maps = Vec::with_capacity(4);
        for (keys, store) in self.direct.keys.iter().zip(&self.direct.maps) {
            let (nodes, _) = store.clone().into_facts().map_err(ExtractError::from)?;
            maps.push(keys.iter().cloned().zip(nodes).collect::<BTreeMap<_, _>>());
        }
        let (rule_nodes, rule_edges) = self
            .direct
            .rules
            .clone()
            .into_facts()
            .map_err(ExtractError::from)?;
        let mut maps = maps.into_iter();
        let mut direct = DirectFacts {
            owners: maps.next().unwrap_or_default(),
            declarations: maps.next().unwrap_or_default(),
            locals: maps.next().unwrap_or_default(),
            supporting: maps.next().unwrap_or_default(),
            rules: rule_nodes.into_iter().zip(rule_edges).collect(),
        };
        if self.commit != commit {
            super::retarget_commit(&mut extraction, commit).map_err(ExtractError::from)?;
            direct.retarget(commit);
        }
        Ok((extraction, direct))
    }
}

/// Attested per-file parses of the captured TS lane (ADR-0087, #438), kept
/// by the host between ingests of one repository. A file is parsed again
/// unless its retained bytes, path, source, repository, registry set and
/// producer contract all equal those of the cached parse; its receipts are
/// always minted afresh over the current capture.
#[derive(Debug, Default)]
pub struct CapturedCache {
    files: BTreeMap<String, AttestedParse>,
}

impl CapturedCache {
    /// Files holding an attested parse.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// True when no file holds an attested parse.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

fn supported(path: &str) -> bool {
    super::SOURCE_EXTENSIONS
        .iter()
        .chain(&super::vue::COMPONENT_EXTENSIONS)
        .any(|extension| path.ends_with(extension))
        && !path.ends_with(".d.ts")
}

fn excluded_directory(name: &str) -> bool {
    super::skipped_directory(name)
}

/// This directory's own `.gitignore`, read through the confined handle
/// without following a symlink. Anything but a regular file is not a rule
/// source, exactly as for the ordinary walk (`source_walk::files`).
fn own_gitignore(directory: &Dir) -> Result<Option<Vec<u8>>, CapturedError> {
    match directory.symlink_metadata(source_walk::GITIGNORE) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = directory.open_with(source_walk::GITIGNORE, &options)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(CapturedError::Invalid("ignore file is not a regular file"));
    }
    match source_walk::read_gitignore(file) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            Err(CapturedError::Limit("ignore file bytes"))
        }
        Err(error) => Err(error.into()),
    }
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= source_capture::MAX_PATH_BYTES
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The selected TS/JS files of a tree, and the symlinks it did not follow.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// Selected source paths, sorted.
    pub files: Vec<String>,
    /// Non-ignored, non-excluded symlinks, sorted. None is followed or
    /// opened, as in the ordinary walk (ADR-0030); callers report them so
    /// what they might hold is never claimed as covered (#1229).
    pub unfollowed: Vec<String>,
    /// Resolution configs directory completion reads (tsconfig, jsconfig,
    /// bundler configs, `package.json`), sorted. A capture retains them with
    /// the files so completion's configs can be bound (ADR-0072 slice 6).
    /// A bundler config is a TS/JS source and is listed in `files` too.
    /// A root `vercel.json`, which completion reads, is listed here as well.
    pub configs: Vec<String>,
}

/// Enumerate the ordinary TS/JS selection without following any symlink.
/// The selection honors the tree's own `.gitignore` files through the same
/// matcher as the ordinary walk (#248), so both lanes select identical files.
/// Each descent uses a rooted no-follow handle, including after a concurrent
/// entry replacement. The subsequent capture independently confines acquisition.
/// Symlinks are skipped and listed in [`Selection::unfollowed`]; unsupported
/// encodings and selected special files fail, and no partial selection is
/// returned. This is not a filesystem snapshot.
pub fn enumerate_selection(root: &Path) -> Result<Selection, CapturedError> {
    enumerate_selection_within(root, SelectionLimits::default())
}

/// Bounds of one TS/JS selection walk a project may change (#1653).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionLimits {
    /// Directory entries examined, including excluded ones; 1 to
    /// [`HARD_MAX_VISITED_ENTRIES`].
    pub max_visited_entries: usize,
    /// Files selected; at most [`source_capture::HARD_MAX_FILES`], and the
    /// capture that follows applies its own file bound.
    pub max_files: usize,
}

impl Default for SelectionLimits {
    fn default() -> Self {
        Self {
            max_visited_entries: MAX_VISITED_ENTRIES,
            max_files: source_capture::MAX_FILES,
        }
    }
}

/// [`enumerate_selection`] within `limits`. Limits above the hard ceilings
/// are refused before the walk starts.
pub fn enumerate_selection_within(
    root: &Path,
    limits: SelectionLimits,
) -> Result<Selection, CapturedError> {
    if !(1..=HARD_MAX_VISITED_ENTRIES).contains(&limits.max_visited_entries)
        || limits.max_files > source_capture::HARD_MAX_FILES
    {
        return Err(CapturedError::Invalid("selection limits"));
    }
    let directory = Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    enumerate_with_limits(
        &directory,
        limits.max_visited_entries,
        MAX_DIRECTORY_DEPTH,
        limits.max_files,
        MAX_UNFOLLOWED_LINKS,
    )
}

/// The selected files of [`enumerate_selection`].
pub fn enumerate_paths(root: &Path) -> Result<Vec<String>, CapturedError> {
    Ok(enumerate_selection(root)?.files)
}

fn enumerate_with_limits(
    directory: &Dir,
    max_entries: usize,
    max_depth: usize,
    max_files: usize,
    max_unfollowed: usize,
) -> Result<Selection, CapturedError> {
    struct Walk {
        files: Vec<String>,
        unfollowed: Vec<String>,
        configs: Vec<String>,
        visited: usize,
        max_entries: usize,
        max_depth: usize,
        max_files: usize,
        max_unfollowed: usize,
    }
    impl Walk {
        fn visit(
            &mut self,
            directory: &Dir,
            prefix: &str,
            parent: &IgnoreRules,
            depth: usize,
        ) -> Result<(), CapturedError> {
            let rules = parent.descend(prefix, own_gitignore(directory)?.as_deref());
            for entry in directory.entries()? {
                let entry = entry?;
                self.visited += 1;
                if self.visited > self.max_entries {
                    return Err(CapturedError::Limit("visited entries"));
                }
                let os_name = entry.file_name();
                let name = os_name
                    .to_str()
                    .ok_or(CapturedError::Invalid("non-UTF-8 path"))?;
                let kind = entry.file_type()?;
                if (kind.is_dir() || kind.is_symlink())
                    && excluded_directory(name)
                    && !supported(name)
                {
                    continue;
                }
                let path = if prefix.is_empty() {
                    name.to_string()
                } else {
                    format!("{prefix}/{name}")
                };
                // Ignored entries are excluded like skipped directories, so
                // an ignored symlink is not part of the selection either.
                if rules.is_ignored(&path, kind.is_dir()) {
                    continue;
                }
                // Never followed, opened or resolved; recorded under the same
                // path validation as a selected file.
                if kind.is_symlink() {
                    if !valid_path(&path) {
                        return Err(CapturedError::Invalid("symlink path"));
                    }
                    if self.unfollowed.len() == self.max_unfollowed {
                        return Err(CapturedError::Limit("unfollowed links"));
                    }
                    self.unfollowed.push(path);
                    continue;
                }
                if kind.is_dir() && excluded_directory(name) {
                    continue;
                }
                if kind.is_dir() {
                    if depth == self.max_depth {
                        return Err(CapturedError::Limit("directory depth"));
                    }
                    if !valid_path(&path) {
                        return Err(CapturedError::Invalid("directory path"));
                    }
                    let child = directory.open_dir_nofollow(name)?;
                    self.visit(&child, &path, &rules, depth + 1)?;
                } else if supported(name) {
                    if !kind.is_file() || !valid_path(&path) {
                        return Err(CapturedError::Invalid("selected source path or type"));
                    }
                    if self.files.len() == self.max_files {
                        return Err(CapturedError::Limit("selected files"));
                    }
                    if is_config_path(&path) {
                        self.configs.push(path.clone());
                    }
                    self.files.push(path);
                } else if (is_config_path(&path) || path == PROBED_ROOT_FILE)
                    && kind.is_file()
                    && valid_path(&path)
                {
                    if self.configs.len() == self.max_files {
                        return Err(CapturedError::Limit("selected files"));
                    }
                    self.configs.push(path);
                }
            }
            Ok(())
        }
    }
    let mut walk = Walk {
        files: Vec::new(),
        unfollowed: Vec::new(),
        configs: Vec::new(),
        visited: 0,
        max_entries,
        max_depth,
        max_files,
        max_unfollowed,
    };
    walk.visit(directory, "", &IgnoreRules::default(), 0)?;
    walk.files.sort();
    walk.unfollowed.sort();
    walk.configs.sort();
    Ok(Selection {
        files: walk.files,
        unfollowed: walk.unfollowed,
        configs: walk.configs,
    })
}

/// Parse precisely this immutable retained buffer, recording only direct lexical
/// rules, their actual emitted owners, lexical GOVERNS, and every other directly
/// emitted callable declaration (#691). No source read or retrospective
/// attestation of a caller-provided Extraction occurs here.
/// Unsupported/oversized receipt inventories omit that whole receipt; they do
/// not truncate ranges or change ordinary recovered facts or interpretation.
///
/// Event sites are recognized under the built-in registries; receipts cover
/// lexical rules and declarations only, so they are identical under any
/// registry set.
pub fn extract_file(
    file: &CapturedFile,
    id: &SourceId,
) -> Result<(Extraction, Vec<Receipt>), CapturedError> {
    let (extraction, receipts) =
        extract_file_with_measurements(file, id, Registries::builtin(), FILE_DECLARATIONS, false)?;
    let mut receipts = receipts
        .rules
        .iter()
        .chain(&receipts.declarations)
        .map(MintedReceipt::to_receipt)
        .collect::<Result<Vec<_>, _>>()?;
    receipts.sort_by(|a, b| a.fact_key().cmp(b.fact_key()));
    Ok((extraction, receipts))
}

/// Explicit producer lane for captured-source size measurements. Historical
/// extraction keeps its receipt bytes; callers selecting this lane must publish
/// the returned receipt identities and use their normal retention limits.
/// Measurement happens before receipt-budget admission, never after publication.
pub fn extract_file_with_source_sizes(
    file: &CapturedFile,
    id: &SourceId,
) -> Result<(Extraction, Vec<Receipt>), CapturedError> {
    let (extraction, receipts) =
        extract_file_with_measurements(file, id, Registries::builtin(), FILE_DECLARATIONS, true)?;
    let mut receipts = receipts
        .rules
        .iter()
        .chain(&receipts.declarations)
        .map(MintedReceipt::to_receipt)
        .collect::<Result<Vec<_>, _>>()?;
    receipts.sort_by(|a, b| a.fact_key().cmp(b.fact_key()));
    Ok((extraction, receipts))
}

/// A completion-decided fact of one file.
enum Fact {
    Node(Node),
    Edge(Edge),
}

/// One file's receipts, split by admission priority. Rule receipts keep their
/// original hard bounds; declaration receipts only use the remaining budget.
struct FileReceipts {
    rules: Vec<MintedReceipt>,
    /// Declarations in fact-key order.
    declarations: Vec<MintedReceipt>,
    /// Supporting File/TestCase receipts in fact-key order, then
    /// unresolved-call Gap receipts in call order (AC-1019), then each
    /// pending call's CALLS and Gap edge in call order (AC-1096). For the
    /// directory lane only: a single file has no completion to decide them.
    completed: Vec<MintedReceipt>,
    /// Declarations never minted because this file reached its own bound.
    omitted_declarations: u64,
    /// Whether this file's parse is proven for this capture: fresh, or
    /// reused under this exact closure (ADR-0072 slice 5).
    proven: bool,
}

const FILE_DECLARATIONS: ReceiptBudget = ReceiptBudget {
    bytes: MAX_FILE_DECLARATION_BYTES,
    receipts: MAX_FILE_DECLARATION_RECEIPTS,
};

#[cfg(test)]
fn extract_file_with(
    file: &CapturedFile,
    id: &SourceId,
    registries: &Registries,
    file_declarations: ReceiptBudget,
) -> Result<(Extraction, FileReceipts), CapturedError> {
    extract_file_with_measurements(file, id, registries, file_declarations, true)
}

fn extract_file_with_measurements(
    file: &CapturedFile,
    id: &SourceId,
    registries: &Registries,
    file_declarations: ReceiptBudget,
    measure_source: bool,
) -> Result<(Extraction, FileReceipts), CapturedError> {
    let (extraction, direct) = parse_file(file, id, registries)?;
    let receipts = mint_file(
        file,
        id,
        &extraction,
        &direct,
        file_declarations,
        measure_source,
    );
    Ok((extraction, receipts))
}

/// Refuse a retained file the TS lane cannot parse as itself.
fn admit_file(file: &CapturedFile, id: &SourceId) -> Result<(), CapturedError> {
    if !supported(&file.reference().path) || !valid_path(&file.reference().path) {
        return Err(CapturedError::Invalid("unsupported captured path"));
    }
    receipt::validate_identity(file.reference(), id.repo)
}

/// Parse exactly this retained buffer, recording its direct facts.
fn parse_file(
    file: &CapturedFile,
    id: &SourceId,
    registries: &Registries,
) -> Result<(Extraction, DirectFacts), CapturedError> {
    admit_file(file, id)?;
    let mut direct = DirectFacts::default();
    let extraction = super::extract_source_recording(
        file.bytes(),
        &file.reference().path,
        id,
        registries,
        Some(&mut direct),
    )?;
    Ok((extraction, direct))
}

/// Mint one file's receipts over its retained buffer from its parse.
fn mint_file(
    file: &CapturedFile,
    id: &SourceId,
    extraction: &Extraction,
    direct: &DirectFacts,
    file_declarations: ReceiptBudget,
    measure_source: bool,
) -> FileReceipts {
    // Every receipt of this file shares one header.
    let mut header = None;
    let mut mint = |receipt: Result<Receipt, CapturedError>| {
        receipt
            .and_then(|receipt| {
                if measure_source {
                    receipt.with_source_sizes(file)
                } else {
                    Ok(receipt)
                }
            })
            .and_then(|receipt| MintedReceipt::compact(receipt, &mut header))
    };
    let mut receipts = BTreeMap::new();
    for (rule, edge) in &direct.rules {
        for node in [Some(rule), direct.owners.get(&edge.dst)]
            .into_iter()
            .flatten()
        {
            if let Ok(receipt) = mint(Receipt::for_node(file, id, node)) {
                receipts.insert(receipt.fact_key().clone(), receipt);
            }
        }
        if let Ok(receipt) = mint(Receipt::for_edge(file, id, edge)) {
            receipts.insert(receipt.fact_key().clone(), receipt);
        }
    }
    // Callables that own no rule are still directly emitted declarations; a
    // provenance-only owner receipt makes their exact span citable (#691).
    // Minting stops at this file's own bound, so a file of many tiny callables
    // cannot allocate unbounded receipts before directory admission; the rest
    // are counted, never minted.
    let mut declarations = Vec::new();
    let mut charge = Charge::default();
    let mut omitted_declarations = 0;
    for (id_key, node) in direct
        .owners
        .iter()
        .chain(&direct.declarations)
        .chain(&direct.locals)
    {
        let key = core_graph::source::FactKey::Node { id: id_key.clone() };
        if receipts.contains_key(&key) {
            continue;
        }
        if declarations.len() == file_declarations.receipts {
            omitted_declarations += 1;
            continue;
        }
        let Ok(receipt) = mint(Receipt::for_node(file, id, node)) else {
            continue;
        };
        if charge.admit(&receipt, declarations.len(), file_declarations) {
            declarations.push(receipt);
        } else {
            omitted_declarations += 1;
        }
    }
    let mut rules: Vec<_> = receipts.into_values().collect();
    retain_matching(&mut rules, extraction);
    let matching = matcher(extraction);
    declarations.retain(|receipt| matching(receipt));
    drop(matching);
    // A gap task also reads the file's File node, its TestCase facts and the
    // unresolved-call Gap itself (AC-1019). Each cites a span this parse read;
    // the Gap exists only once directory completion finds no target, and
    // completion may enrich the others, so these receipts go to the directory
    // lane, which keeps one only when its complete fact survives completion.
    // They use what is left of this file's declaration bound and, being
    // undecided, are never counted as omissions.
    let gaps = extraction
        .pending_calls
        .iter()
        .filter_map(|pending| pending.gap_node.unpack().ok());
    // A pending call's outcome is decided by the walk: its CALLS edge when
    // the target is found as parsed, else its Gap edge (ADR-0072 slice 6).
    // Both are minted here and completion keeps only the one emitted
    // unchanged; a call resolved through a re-export, a base or an
    // interface is emitted with other evidence and keeps no receipt.
    let calls = extraction.pending_calls.iter().flat_map(|pending| {
        [&pending.resolved_edge, &pending.gap_edge]
            .into_iter()
            .filter_map(|edge| edge.unpack().ok())
    });
    let mut completed = Vec::new();
    let facts = direct
        .supporting
        .values()
        .cloned()
        .chain(gaps)
        .map(Fact::Node)
        .chain(calls.map(Fact::Edge));
    for fact in facts {
        let held = declarations.len() + completed.len();
        if held == file_declarations.receipts {
            break;
        }
        let receipt = match &fact {
            Fact::Node(node) => Receipt::for_node(file, id, node),
            Fact::Edge(edge) => Receipt::for_edge(file, id, edge),
        };
        let Ok(receipt) = mint(receipt) else {
            continue;
        };
        if charge.admit(&receipt, held, file_declarations) {
            completed.push(receipt);
        }
    }
    FileReceipts {
        rules,
        declarations,
        completed,
        omitted_declarations,
        proven: false,
    }
}

/// Parse the selected immutable membership, bypassing the ordinary parse cache,
/// then run the exact same directory completion as ordinary extraction. `root`
/// is used by that separate live configuration pass, not to read primary files.
/// The host persists this capture and receipts before graph publication, and
/// must repeat complete-fact matching after its own cross-layer enrichments.
///
/// Per-file parsing runs on the same ordered worker pool as the uncaptured
/// lane (#236, #482): files parse concurrently on up to [`workers`] threads,
/// but every result is merged back on the calling thread strictly in
/// manifest order (the manifest is itself sorted by path, #248), so receipts
/// and facts are byte-identical to a serial run for any worker count
/// (AC-0217) — parallelism changes only wall-clock time.
///
/// Event sites are recognized under `registries`, the ingest's one
/// effective registry set (#594).
///
/// [`workers`]: source_walk::parallel::workers
pub fn extract_captured_dir(
    root: &Path,
    id: &SourceId,
    capture: &Capture,
    registries: &Registries,
    on_file: &mut dyn FnMut(&str),
) -> Result<(Extraction, Vec<MintedReceipt>, IncrementalStats), CapturedError> {
    extract_captured_dir_bounded(
        root,
        id,
        capture,
        registries,
        on_file,
        ReceiptBudget {
            bytes: MAX_EXTRACTION_RECEIPT_BYTES,
            receipts: MAX_EXTRACTION_RECEIPTS,
        },
        FILE_DECLARATIONS,
    )
}

/// A packed captured extraction: the extraction's sites (its `nodes` and
/// `edges` empty), the facts packed in manifest order, the admitted receipts
/// and the walk's statistics.
pub type PackedCapturedExtraction = (Extraction, FactStore, Vec<MintedReceipt>, IncrementalStats);

/// [`extract_captured_dir`] without the final decode. Each file's facts are
/// packed as its parse is merged, directory completion and receipt matching
/// decode only the facts they read, and the result comes back packed. No
/// whole-repository props tree is ever held, which is what bounds the
/// rule-evidence peak on large sources (#1237, #725). Facts, receipts and
/// omissions are identical to [`extract_captured_dir`].
pub fn extract_captured_dir_packed(
    root: &Path,
    id: &SourceId,
    capture: &Capture,
    registries: &Registries,
    on_file: &mut dyn FnMut(&str),
) -> Result<PackedCapturedExtraction, CapturedError> {
    extract_captured_dir_packed_with_policy(
        root,
        id,
        capture,
        registries,
        None,
        on_file,
        DirectoryReceiptPolicy {
            budget: ReceiptBudget {
                bytes: MAX_EXTRACTION_RECEIPT_BYTES,
                receipts: MAX_EXTRACTION_RECEIPTS,
            },
            file_declarations: FILE_DECLARATIONS,
            measure_source: true,
        },
    )
}

/// [`extract_captured_dir_packed`] reusing the attested parse of each file
/// whose retained bytes and producer inputs are unchanged since `cache` was
/// filled (ADR-0087, #438). Reused files are not parsed; their receipts
/// are minted afresh over this capture, so facts, cited spans and omissions
/// equal a cold walk's. A reused parse proves its input closure only when it
/// was made under that exact closure; otherwise its rule and
/// declaration receipts are `cache_reuse_unproven` where a cold walk's are
/// established (ADR-0072 slice 5). The statistics count reused, parsed and deleted
/// files, and `cache` ends holding exactly this walk's parses. A failed
/// walk leaves it empty.
pub fn extract_captured_dir_packed_cached(
    root: &Path,
    id: &SourceId,
    capture: &Capture,
    registries: &Registries,
    cache: &mut CapturedCache,
    on_file: &mut dyn FnMut(&str),
) -> Result<PackedCapturedExtraction, CapturedError> {
    extract_captured_dir_packed_with_policy(
        root,
        id,
        capture,
        registries,
        Some(cache),
        on_file,
        DirectoryReceiptPolicy {
            budget: ReceiptBudget {
                bytes: MAX_EXTRACTION_RECEIPT_BYTES,
                receipts: MAX_EXTRACTION_RECEIPTS,
            },
            file_declarations: FILE_DECLARATIONS,
            measure_source: true,
        },
    )
}

/// Extraction receipt ceilings; tests narrow them to exercise admission order.
#[derive(Clone, Copy)]
struct ReceiptBudget {
    bytes: usize,
    receipts: usize,
}

/// Retained memory of admitted receipts (#725): each receipt's own bytes, and
/// each shared file header and input closure record once, charged with the
/// first receipt admitted under it. Charged headers are held until the charge is dropped, so a freed
/// header's address can never be reused by another uncharged header. Each
/// held header also charges one map entry; B-tree node overhead beyond that
/// is excluded.
#[derive(Default)]
struct Charge {
    bytes: usize,
    headers: BTreeMap<usize, Arc<MintedHeader>>,
    /// Shared input closure records, each charged once at its canonical
    /// size, like headers (AC-1084).
    closures: BTreeMap<usize, Arc<InputClosureV1>>,
}

impl Charge {
    /// Admit `receipt` next to `held` already admitted ones when both
    /// ceilings allow it; a refused receipt charges nothing.
    fn admit(&mut self, receipt: &MintedReceipt, held: usize, budget: ReceiptBudget) -> bool {
        let header = receipt.header();
        let key = Arc::as_ptr(header) as usize;
        let shared = if self.headers.contains_key(&key) {
            0
        } else {
            header.retained_bytes() + std::mem::size_of::<(usize, Arc<MintedHeader>)>()
        };
        let record = receipt
            .shared_input_closure()
            .filter(|record| !self.closures.contains_key(&(Arc::as_ptr(record) as usize)));
        let record_bytes = record.map_or(0, |record| {
            std::mem::size_of::<InputClosureV1>()
                + std::mem::size_of::<(usize, Arc<InputClosureV1>)>()
                + record
                    .canonical_bytes()
                    .map_or(MAX_CLOSURE_BYTES, |bytes| bytes.len())
        });
        let bytes = receipt.retained_bytes() + shared + record_bytes;
        if self.bytes + bytes > budget.bytes || held == budget.receipts {
            return false;
        }
        self.bytes += bytes;
        self.headers
            .entry(key)
            .or_insert_with(|| Arc::clone(header));
        if let Some(record) = record {
            self.closures
                .insert(Arc::as_ptr(record) as usize, Arc::clone(record));
        }
        true
    }
}

fn extract_captured_dir_bounded(
    root: &Path,
    id: &SourceId,
    capture: &Capture,
    registries: &Registries,
    on_file: &mut dyn FnMut(&str),
    budget: ReceiptBudget,
    file_declarations: ReceiptBudget,
) -> Result<(Extraction, Vec<MintedReceipt>, IncrementalStats), CapturedError> {
    extract_captured_dir_with_policy(
        root,
        id,
        capture,
        registries,
        on_file,
        DirectoryReceiptPolicy {
            budget,
            file_declarations,
            measure_source: true,
        },
    )
}

struct DirectoryReceiptPolicy {
    budget: ReceiptBudget,
    file_declarations: ReceiptBudget,
    measure_source: bool,
}

fn extract_captured_dir_with_policy(
    root: &Path,
    id: &SourceId,
    capture: &Capture,
    registries: &Registries,
    on_file: &mut dyn FnMut(&str),
    policy: DirectoryReceiptPolicy,
) -> Result<(Extraction, Vec<MintedReceipt>, IncrementalStats), CapturedError> {
    let (mut extraction, facts, receipts, stats) = extract_captured_dir_packed_with_policy(
        root, id, capture, registries, None, on_file, policy,
    )?;
    (extraction.nodes, extraction.edges) = facts.into_facts().map_err(ExtractError::from)?;
    Ok((extraction, receipts, stats))
}

fn extract_captured_dir_packed_with_policy(
    root: &Path,
    id: &SourceId,
    capture: &Capture,
    registries: &Registries,
    // Absent, nothing is reused or retained.
    mut cache: Option<&mut CapturedCache>,
    on_file: &mut dyn FnMut(&str),
    policy: DirectoryReceiptPolicy,
) -> Result<PackedCapturedExtraction, CapturedError> {
    let DirectoryReceiptPolicy {
        budget,
        file_declarations,
        measure_source,
    } = policy;
    receipt::validate_source_repo(&capture.manifest().source_id, id.repo)?;
    // A capture may also hold the Python lane's files (AC-1024); this walk
    // reads only the TS/JS selection.
    let paths: Vec<String> = capture
        .manifest()
        .files
        .iter()
        .filter(|entry| supported(&entry.path))
        .map(|entry| entry.path.clone())
        .collect();
    let mut extraction = Extraction::default();
    // Facts are packed file by file (#1237): the walk never holds more decoded
    // props than the ordered worker window produces.
    let mut store = FactStore::default();
    let mut receipts = Vec::new();
    let mut charge = Charge::default();
    // Declarations are admitted after every rule receipt, so they can never
    // turn an extraction that fit before #691 into an extraction limit failure
    // (host store capacity remains separately authoritative). Retention
    // stops once they alone could not fit, bounding the extra memory.
    let mut declarations = Vec::new();
    let mut staged = Charge::default();
    // Completion-decided File/TestCase/Gap receipts (AC-1019) are admitted
    // last, after every declaration. They are no declaration contract: one
    // that does not fit is left out unnamed and its fact stays uncaptured.
    let mut completed = Vec::new();
    let mut staged_completed = Charge::default();
    // Paths whose parse this capture does not prove (ADR-0072 slice 5).
    let mut unproven = std::collections::BTreeSet::new();
    let mut omitted_rules = Vec::new();
    let mut stats = IncrementalStats::default();
    // The cache is taken whole, so a failed walk leaves nothing behind.
    // Workers read a file's previous parse; its merge then moves it out, so
    // the walk holds about one parse per file, never two whole caches.
    let retain = cache.is_some();
    let previous: BTreeMap<String, std::sync::Mutex<Option<AttestedParse>>> = cache
        .as_deref_mut()
        .map(|cache| std::mem::take(&mut cache.files))
        .unwrap_or_default()
        .into_iter()
        .map(|(path, parse)| (path, std::sync::Mutex::new(Some(parse))))
        .collect();
    let active: std::collections::BTreeSet<&str> = paths.iter().map(String::as_str).collect();
    stats.deleted_files = previous
        .keys()
        .filter(|path| !active.contains(path.as_str()))
        .count() as u64;
    let registry_identity = registries.identity();
    let mut next = BTreeMap::new();
    source_walk::parallel::map_ordered(
        &paths,
        |path| {
            let file = capture.file(path)?;
            admit_file(file, id)?;
            let attestation = Attestation::of(file, id.repo, &registry_identity);
            let slot = previous
                .get(path)
                .map(|slot| slot.lock().unwrap_or_else(|p| p.into_inner()));
            let prior = slot
                .as_deref()
                .and_then(Option::as_ref)
                .filter(|prior| prior.attestation == attestation);
            let record = direct_lexical_closure(file.reference(), id.repo)?;
            // `keep` is whether a reused parse was made at this commit;
            // `made_under` is the closure the parse was made under.
            let (extraction, direct, keep, made_under) = match prior {
                Some(prior) => {
                    let (extraction, direct) = prior.unpack(id.commit)?;
                    let keep = Some(prior.commit == id.commit);
                    (extraction, direct, keep, prior.closure_id.clone())
                }
                None => {
                    let (extraction, direct) = parse_file(file, id, registries)?;
                    (extraction, direct, None, record.closure_id.clone())
                }
            };
            drop(slot);
            let receipts = mint_file(
                file,
                id,
                &extraction,
                &direct,
                file_declarations,
                measure_source,
            );
            // A fresh parse's direct-lexical facts are a function of this
            // file's bytes alone. A reused parse proves that only when it was
            // made under this exact closure (ADR-0072); a commit retarget
            // reads no input.
            let proven = made_under == record.closure_id;
            let closure = proven.then(|| Arc::new(record));
            let receipts = establish(receipts, closure)?;
            // A parse reused at its own commit is moved on as it is; any
            // other is packed afresh for the next walk.
            let attested = if retain && keep != Some(true) {
                Some(AttestedParse::pack(
                    attestation,
                    id.commit,
                    made_under,
                    &extraction,
                    direct,
                )?)
            } else {
                None
            };
            Ok::<_, CapturedError>((extraction, receipts, attested, keep.is_some()))
        },
        |path, (mut facts, file_receipts, attested, reused)| {
            on_file(path);
            let prior = previous
                .get(path)
                .and_then(|slot| slot.lock().unwrap_or_else(|p| p.into_inner()).take());
            if retain && let Some(parse) = attested.or(prior.filter(|_| reused)) {
                next.insert(path.to_owned(), parse);
            }
            if reused {
                stats.reused_files += 1;
            } else {
                stats.recomputed_files += 1;
            }
            // Rule receipts are admitted greedily in manifest order (#722). One
            // that does not fit is omitted by fact key, never retained, so a
            // large repository degrades to named uncitable facts instead of
            // failing, and memory stays within the budget.
            for receipt in file_receipts.rules {
                if charge.admit(&receipt, receipts.len(), budget) {
                    receipts.push(receipt);
                } else {
                    omitted_rules.push((
                        path.to_owned(),
                        receipt.fact_key().clone(),
                        receipt.fact_digest().to_owned(),
                    ));
                }
            }
            if file_receipts.omitted_declarations > 0 {
                *stats
                    .omitted_declaration_receipts
                    .entry(path.to_owned())
                    .or_default() += file_receipts.omitted_declarations;
                stats.omitted_receipt_facts_complete = false;
            }
            for receipt in file_receipts.declarations {
                if staged.admit(&receipt, declarations.len(), budget) {
                    declarations.push(receipt);
                } else {
                    *stats
                        .omitted_declaration_receipts
                        .entry(path.to_owned())
                        .or_default() += 1;
                    stats
                        .omitted_receipt_facts
                        .push(core_graph::source::SourceOmission {
                            fact: receipt.fact_key().clone(),
                            emitted_fact_digest: receipt.fact_digest().to_owned(),
                            kind: core_graph::source::ReceiptOmissionKind::Declarations,
                        });
                }
            }
            if !file_receipts.proven {
                unproven.insert(path.to_owned());
            }
            for receipt in file_receipts.completed {
                if staged_completed.admit(&receipt, completed.len(), budget) {
                    completed.push((receipt, file_receipts.proven));
                }
            }
            store.extend_packed(
                FactStore::from_facts(
                    std::mem::take(&mut facts.nodes),
                    std::mem::take(&mut facts.edges),
                )
                .map_err(ExtractError::from)?,
            );
            super::append_extraction(&mut extraction, facts);
            Ok(())
        },
    )?;
    drop(previous);
    // Completion still reads its configs live; they are bound only when the
    // capture retains exactly the bytes read (ADR-0072 slice 6).
    let (index, reads) =
        super::resolution::ResolutionIndex::load_reading(root).map_err(ExtractError::from)?;
    stats.resolution_configs = bind_configs(capture, &reads);
    drop(reads);
    let mut probes = super::Probes::default();
    // Completion only edits held facts in place and appends new ones, so
    // every fact past these counts is one it emitted.
    let held = (store.nodes().len(), store.edges().len());
    super::complete_directory_with(&mut extraction, &mut store, root, id, &index, &mut probes)?;
    drop(index);
    stats.completion_probes = bind_probes(capture, &probes);
    drop(probes);
    let receipted: std::collections::BTreeSet<FactKey> = receipts
        .iter()
        .chain(&declarations)
        .chain(completed.iter().map(|(receipt, _)| receipt))
        .map(|receipt| receipt.fact_key().clone())
        .collect();
    let emitted = completion_receipts(capture, id, &store, held, &receipted, budget);
    drop(receipted);
    let matching = packed_matcher(&store);
    receipts.retain(|receipt| matching(receipt));
    // Count an omitted rule only when its fact survived directory completion,
    // so the named omissions describe facts the graph actually holds.
    let surviving = surviving_facts(&store);
    for (path, key, digest) in omitted_rules {
        if surviving(&key) {
            *stats.omitted_rule_receipts.entry(path).or_default() += 1;
            stats
                .omitted_receipt_facts
                .push(core_graph::source::SourceOmission {
                    fact: key,
                    emitted_fact_digest: digest,
                    kind: core_graph::source::ReceiptOmissionKind::Rules,
                });
        }
    }
    drop(surviving);
    // Only declarations whose complete fact survived directory completion use
    // budget. Admission is greedy in deterministic manifest order: a declaration
    // that does not fit is skipped and a smaller later one may still be admitted.
    drop(staged);
    for receipt in declarations {
        if !matching(&receipt) {
            continue;
        }
        if charge.admit(&receipt, receipts.len(), budget) {
            receipts.push(receipt);
        } else {
            *stats
                .omitted_declaration_receipts
                .entry(receipt.file().path.clone())
                .or_default() += 1;
            stats
                .omitted_receipt_facts
                .push(core_graph::source::SourceOmission {
                    fact: receipt.fact_key().clone(),
                    emitted_fact_digest: receipt.fact_digest().to_owned(),
                    kind: core_graph::source::ReceiptOmissionKind::Declarations,
                });
        }
    }
    drop(staged_completed);
    // A completion-decided receipt is established on the walk's closure
    // only when completion's configs and lookups are bound to the capture
    // (ADR-0072 slice 6); one closure per file, shared by its receipts.
    // A file's decided receipts are contiguous, so only the current file's
    // record is held here; an admitted receipt keeps its own, which the
    // budget charges.
    let walk = walk_inputs(capture, &stats);
    let mut current = None;
    for (receipt, proven) in completed {
        if !matching(&receipt) {
            continue;
        }
        let receipt = state_completed(receipt, proven, walk.as_deref(), id.repo, &mut current)?;
        if charge.admit(&receipt, receipts.len(), budget) {
            receipts.push(receipt);
        }
    }
    drop(current);
    // Facts completion emitted itself that cite one TS file (Pulumi
    // references, among others) are stated on the same walk closure, file
    // by file, when that file's parse is proven (AC-1098).
    let mut current = None;
    for receipt in emitted {
        if !matching(&receipt) {
            continue;
        }
        let proven = !unproven.contains(&receipt.file().path);
        let receipt = state_completed(receipt, proven, walk.as_deref(), id.repo, &mut current)?;
        if charge.admit(&receipt, receipts.len(), budget) {
            receipts.push(receipt);
        }
    }
    drop(current);
    drop(matching);
    // Installed only once every fallible step has succeeded.
    if let Some(cache) = cache {
        cache.files = next;
    }
    Ok((extraction, store, receipts, stats))
}

/// Receipt candidates for the facts directory completion appended past
/// `held` (nodes, edges), skipping any fact already `receipted`. Each is
/// minted against the captured TS file its first evidence entry cites, file
/// by file in path order, and staged within `budget`. A TS receipt binds one
/// file, so a fact whose evidence also cites another file (a barrel, a
/// `package.json`) gets none, as does one that does not fit. Facts are
/// decoded one file at a time.
fn completion_receipts(
    capture: &Capture,
    id: &SourceId,
    store: &FactStore,
    (nodes, edges): (usize, usize),
    receipted: &std::collections::BTreeSet<FactKey>,
    budget: ReceiptBudget,
) -> Vec<MintedReceipt> {
    let mut by_file: BTreeMap<String, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    let primary = |props: &serde_json::Value| {
        python::first_evidence_path(props)
            .filter(|path| supported(path))
            .map(str::to_owned)
    };
    for (index, node) in store.nodes().iter().enumerate().skip(nodes) {
        let Ok(node) = node.unpack() else { continue };
        if receipted.contains(&FactKey::Node {
            id: node.id.clone(),
        }) {
            continue;
        }
        if let Some(path) = primary(&node.props) {
            by_file.entry(path).or_default().0.push(index);
        }
    }
    for (index, edge) in store.edges().iter().enumerate().skip(edges) {
        let Ok(edge) = edge.unpack() else { continue };
        if receipted.contains(&FactKey::Edge {
            source: edge.src.clone(),
            label: edge.label.clone(),
            destination: edge.dst.clone(),
        }) {
            continue;
        }
        if let Some(path) = primary(&edge.props) {
            by_file.entry(path).or_default().1.push(index);
        }
    }
    let mut staged = Vec::new();
    let mut charge = Charge::default();
    let mut seen = std::collections::BTreeSet::new();
    for (path, (node_slots, edge_slots)) in by_file {
        let Ok(file) = capture.file(&path) else {
            continue;
        };
        let decoded_nodes: Vec<Node> = node_slots
            .iter()
            .filter_map(|&index| store.nodes()[index].unpack().ok())
            .collect();
        let decoded_edges: Vec<Edge> = edge_slots
            .iter()
            .filter_map(|&index| store.edges()[index].unpack().ok())
            .collect();
        let Ok(minted) = python::mint_lane_receipts_in(
            Grammar::for_path(&path),
            file,
            None,
            id,
            &decoded_nodes,
            &decoded_edges,
        ) else {
            continue;
        };
        for receipt in minted {
            // A key completion emitted twice keeps the receipt minted first;
            // the walk's matching keeps it only if it is the fact held.
            if seen.insert(receipt.fact_key().clone())
                && charge.admit(&receipt, staged.len(), budget)
            {
                staged.push(receipt);
            }
        }
    }
    staged
}

/// Whether directory completion's resolution configs are the capture's own
/// bytes (ADR-0072 slice 6). Completion reads tsconfig, jsconfig, bundler
/// and `package.json` files from the live tree; a later closure may name
/// them only when this is [`ConfigBinding::Bound`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ConfigBinding {
    /// Not a captured walk, or the configs read differ from the capture's.
    #[default]
    Unbound,
    /// Every config read is retained with exactly the bytes read, and the
    /// capture retains no other: one reference per config, sorted by path.
    Bound(Vec<CaptureFileRef>),
}

/// Bind the configs completion read to `capture`. The capture's own config
/// set must be exactly the set read, byte for byte, so a config the live
/// walk did not see, or one changed since capture, leaves them unbound.
fn bind_configs(capture: &Capture, reads: &[super::resolution::ConfigRead]) -> ConfigBinding {
    let captured: Vec<&str> = capture
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .filter(|path| is_config_path(path))
        .collect();
    if captured.len() != reads.len() {
        return ConfigBinding::Unbound;
    }
    let mut bound = Vec::with_capacity(reads.len());
    for (path, (read, bytes)) in captured.iter().zip(reads) {
        if path != read {
            return ConfigBinding::Unbound;
        }
        match capture.file(read) {
            Ok(file) if file.bytes() == bytes.as_slice() => bound.push(file.reference().clone()),
            _ => return ConfigBinding::Unbound,
        }
    }
    ConfigBinding::Bound(bound)
}

/// Whether the capture answers every other live lookup directory completion
/// made exactly as the live tree did (ADR-0072 slice 6). A later closure may
/// name completion's lookups only when this is [`ProbeBinding::Bound`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProbeBinding {
    /// Not a captured walk, or some lookup's live answer is not the
    /// capture's: a path found that the capture does not hold, one the
    /// capture holds that was not found, a root file read with other bytes,
    /// or a dependency type allowlist, under which completion may read
    /// installed packages.
    #[default]
    Unbound,
    /// Every lookup is answered by the capture.
    Bound {
        /// Paths looked up and not found, each absent from the capture,
        /// sorted.
        absent: Vec<String>,
        /// Root files read, each retained with exactly the bytes read,
        /// sorted by path.
        read: Vec<CaptureFileRef>,
    },
}

/// Bind completion's live lookups to `capture`. A directory is held when
/// the capture retains a file beneath it, a file when the capture retains
/// it; a path found and held is covered by the capture's membership.
fn bind_probes(capture: &Capture, probes: &super::Probes) -> ProbeBinding {
    if probes.allowlist {
        return ProbeBinding::Unbound;
    }
    let files = &capture.manifest().files;
    let holds_file = |path: &str| {
        files
            .binary_search_by(|entry| entry.path.as_str().cmp(path))
            .is_ok()
    };
    let holds_directory = |path: &str| {
        let prefix = format!("{path}/");
        let start = files.partition_point(|entry| entry.path.as_str() < prefix.as_str());
        files
            .get(start)
            .is_some_and(|entry| entry.path.starts_with(&prefix))
    };
    let mut absent = Vec::new();
    for (path, (lookup, found)) in &probes.exists {
        let held = match lookup {
            super::Lookup::Directory => holds_directory(path),
            super::Lookup::File => holds_file(path),
        };
        if held != *found {
            return ProbeBinding::Unbound;
        }
        if !found {
            // Absent means the capture holds nothing at the path, neither
            // a file nor anything beneath it.
            if holds_file(path) || holds_directory(path) {
                return ProbeBinding::Unbound;
            }
            absent.push(path.clone());
        }
    }
    let mut read = Vec::new();
    for (path, bytes) in &probes.read {
        match (bytes, capture.file(path)) {
            (None, Err(_)) if !holds_file(path) => absent.push(path.clone()),
            (Some(bytes), Ok(file)) if file.bytes() == bytes.as_slice() => {
                read.push(file.reference().clone());
            }
            _ => return ProbeBinding::Unbound,
        }
    }
    absent.sort();
    absent.dedup();
    ProbeBinding::Bound { absent, read }
}

/// The inputs every completion-decided fact of this walk shares, when its
/// configs and lookups are bound: the capture's enumeration, each config
/// and root file read, and each path looked up and absent.
fn walk_inputs(capture: &Capture, stats: &IncrementalStats) -> Option<Vec<ClosureInput>> {
    let (ConfigBinding::Bound(configs), ProbeBinding::Bound { absent, read }) =
        (&stats.resolution_configs, &stats.completion_probes)
    else {
        return None;
    };
    let source_id = capture.manifest().source_id.clone();
    let capture_id = capture.id().to_string();
    let mut inputs = vec![ClosureInput::Enumeration {
        source_id: source_id.clone(),
        capture_id: capture_id.clone(),
        walk_policy: TS_DIRECTORY_WALK_POLICY.into(),
    }];
    inputs.extend(
        configs
            .iter()
            .chain(read)
            .map(|file| ClosureInput::CapturedFile { file: file.clone() }),
    );
    inputs.extend(
        absent
            .iter()
            .map(|path| ClosureInput::AbsentFromEnumeration {
                source_id: source_id.clone(),
                capture_id: capture_id.clone(),
                path: path.clone(),
            }),
    );
    Some(inputs)
}

/// Record a completion-decided receipt's closure state: established on its
/// file's walk closure when the walk's inputs are bound and its parse is
/// proven; `cache_reuse_unproven` for a parse that is not;
/// `cross_file_input_unbound` when completion read what the capture does
/// not bind; `limit_exceeded` when the closure would pass its bound.
fn state_completed(
    receipt: MintedReceipt,
    proven: bool,
    walk: Option<&[ClosureInput]>,
    repo_key: &str,
    current: &mut Option<(String, Option<Arc<InputClosureV1>>)>,
) -> Result<MintedReceipt, CapturedError> {
    let gap = |reason| ReceiptClosure::InputClosureNotEstablished {
        reasons: vec![reason],
    };
    if !proven {
        return receipt.with_closure(gap(ClosureGapReason::CacheReuseUnproven), None);
    }
    let Some(walk) = walk else {
        return receipt.with_closure(gap(ClosureGapReason::CrossFileInputUnbound), None);
    };
    let file = receipt.file().clone();
    let record = match current {
        Some((path, record)) if *path == file.path => record.clone(),
        _ => {
            let record = match directory_walk_closure(&file, repo_key, walk.to_vec()) {
                Ok(record) => Some(Arc::new(record)),
                Err(CapturedError::Limit(_)) => None,
                Err(error) => return Err(error),
            };
            *current = Some((file.path.clone(), record.clone()));
            record
        }
    };
    match record {
        Some(record) => receipt.with_closure(
            ReceiptClosure::Established {
                closure_id: record.closure_id.clone(),
            },
            Some(record),
        ),
        None => receipt.with_closure(gap(ClosureGapReason::LimitExceeded), None),
    }
}

/// The root file directory completion reads besides its resolution configs.
const PROBED_ROOT_FILE: &str = "vercel.json";

/// True for a path whose file name the resolution index reads.
fn is_config_path(path: &str) -> bool {
    super::resolution::is_config(path.rsplit('/').next().unwrap_or(path))
}

/// Record `receipts`' closure states under contract v4 (ADR-0072 slice 5).
/// A rule or declaration is established by `closure`, its file's own, or is
/// `cache_reuse_unproven` when a reused parse does not prove it. A
/// completion-decided receipt depends on other files of the walk, so its
/// state is decided after completion ([`state_completed`]).
fn establish(
    receipts: FileReceipts,
    closure: Option<Arc<InputClosureV1>>,
) -> Result<FileReceipts, CapturedError> {
    let direct = match &closure {
        Some(closure) => ReceiptClosure::Established {
            closure_id: closure.closure_id.clone(),
        },
        None => ReceiptClosure::InputClosureNotEstablished {
            reasons: vec![ClosureGapReason::CacheReuseUnproven],
        },
    };
    let with = |receipts: Vec<MintedReceipt>,
                state: &ReceiptClosure,
                record: Option<&Arc<InputClosureV1>>| {
        receipts
            .into_iter()
            .map(|receipt| receipt.with_closure(state.clone(), record.cloned()))
            .collect::<Result<Vec<_>, _>>()
    };
    Ok(FileReceipts {
        rules: with(receipts.rules, &direct, closure.as_ref())?,
        declarations: with(receipts.declarations, &direct, closure.as_ref())?,
        completed: receipts.completed,
        omitted_declarations: receipts.omitted_declarations,
        proven: closure.is_some(),
    })
}

fn retain_matching(receipts: &mut Vec<impl Citable>, extraction: &Extraction) {
    let matching = matcher(extraction);
    receipts.retain(|receipt| matching(receipt));
}

/// Fact identities the final packed extraction holds.
fn surviving_facts(facts: &FactStore) -> impl Fn(&FactKey) -> bool + '_ {
    let nodes: std::collections::BTreeSet<&str> =
        facts.nodes().iter().map(|node| node.id.as_str()).collect();
    let edges: std::collections::BTreeSet<(&str, &str, &str)> = facts
        .edges()
        .iter()
        .map(|edge| (edge.src.as_str(), edge.label.as_str(), edge.dst.as_str()))
        .collect();
    move |key| match key {
        FactKey::Node { id } => nodes.contains(id.as_str()),
        FactKey::Edge {
            source,
            label,
            destination,
        } => edges.contains(&(source.as_str(), label.as_str(), destination.as_str())),
    }
}

/// [`matcher`] over the packed final extraction: complete-fact equality, never
/// id-only lookup. Only the one fact a receipt names is decoded, and the last
/// occurrence of an identity decides, as in the list form.
fn packed_matcher<R: Citable>(facts: &FactStore) -> impl Fn(&R) -> bool + '_ {
    let nodes: BTreeMap<&str, &PackedNode> = facts
        .nodes()
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let edges: BTreeMap<(&str, &str, &str), &PackedEdge> = facts
        .edges()
        .iter()
        .map(|edge| {
            (
                (edge.src.as_str(), edge.label.as_str(), edge.dst.as_str()),
                edge,
            )
        })
        .collect();
    move |receipt| match receipt.fact_key() {
        FactKey::Node { id } => nodes
            .get(id.as_str())
            .and_then(|node| node.unpack().ok())
            .is_some_and(|node| receipt.matches_node(&node)),
        FactKey::Edge {
            source,
            label,
            destination,
        } => edges
            .get(&(source.as_str(), label.as_str(), destination.as_str()))
            .and_then(|edge| edge.unpack().ok())
            .is_some_and(|edge| receipt.matches_edge(&edge)),
    }
}

/// Complete-fact equality against one file's own extraction.
fn matcher<R: Citable>(extraction: &Extraction) -> impl Fn(&R) -> bool + '_ {
    let nodes: BTreeMap<_, _> = extraction
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect();
    let edges: BTreeMap<_, _> = extraction
        .edges
        .iter()
        .map(|edge| {
            (
                (edge.src.clone(), edge.label.clone(), edge.dst.clone()),
                edge,
            )
        })
        .collect();
    move |receipt| match receipt.fact_key() {
        core_graph::source::FactKey::Node { id } => {
            nodes.get(id).is_some_and(|node| receipt.matches_node(node))
        }
        core_graph::source::FactKey::Edge {
            source,
            label,
            destination,
        } => edges
            .get(&(source.clone(), label.clone(), destination.clone()))
            .is_some_and(|edge| receipt.matches_edge(edge)),
    }
}

#[cfg(test)]
mod config_binding_tests;
#[cfg(test)]
mod probe_binding_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod v4_lane_tests;
#[cfg(test)]
mod walk_closure_tests;
