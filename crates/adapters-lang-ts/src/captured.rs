//! Producer-bound primary source receipts (SPEC-06).
//!
//! The captured factory invokes the actual parser on the retained byte slice.
//! Only its private direct-output sink admits facts; public receipt validation
//! checks content integrity and does not establish who produced supplied JSON.
//! Directory enrichment still reads live configuration and is outside closure.

use super::{ExtractError, Extraction, IncrementalStats, SourceId};
use adapters_fw::events::Registries;
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, OpenOptions};
use core_graph::facts::FactStore;
use core_graph::packed::{PackedEdge, PackedNode};
use core_graph::source::FactKey;
use core_graph::{Edge, Node};
use source_capture::{Capture, CapturedFile};
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
mod observed;
pub use observed::mint_observed_receipts;
mod receipt;
pub use receipt::{
    Citable, Grammar, MAX_RECEIPT_BYTES, MAX_RECEIPT_OTHER_FILES, MAX_RECEIPT_RANGES, MintedHeader,
    MintedReceipt, RangeRole, Receipt, ReceiptRange, ReceiptSourceSizes, fact_match_workspace,
    fact_match_workspace_bytes, is_input_source,
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
                    self.files.push(path);
                }
            }
            Ok(())
        }
    }
    let mut walk = Walk {
        files: Vec::new(),
        unfollowed: Vec::new(),
        visited: 0,
        max_entries,
        max_depth,
        max_files,
        max_unfollowed,
    };
    walk.visit(directory, "", &IgnoreRules::default(), 0)?;
    walk.files.sort();
    walk.unfollowed.sort();
    Ok(Selection {
        files: walk.files,
        unfollowed: walk.unfollowed,
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

/// One file's receipts, split by admission priority. Rule receipts keep their
/// original hard bounds; declaration receipts only use the remaining budget.
struct FileReceipts {
    rules: Vec<MintedReceipt>,
    /// Declarations in fact-key order.
    declarations: Vec<MintedReceipt>,
    /// Supporting File/TestCase receipts in fact-key order, then
    /// unresolved-call Gap receipts in call order (AC-1019). For the
    /// directory lane only: a single file has no completion to decide them.
    completed: Vec<MintedReceipt>,
    /// Declarations never minted because this file reached its own bound.
    omitted_declarations: u64,
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
    if !supported(&file.reference().path) || !valid_path(&file.reference().path) {
        return Err(CapturedError::Invalid("unsupported captured path"));
    }
    receipt::validate_identity(file.reference(), id.repo)?;
    let mut direct = DirectFacts::default();
    let extraction = super::extract_source_recording(
        file.bytes(),
        &file.reference().path,
        id,
        registries,
        Some(&mut direct),
    )?;
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
    retain_matching(&mut rules, &extraction);
    let matching = matcher(&extraction);
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
    let mut completed = Vec::new();
    for node in direct.supporting.values().cloned().chain(gaps) {
        let held = declarations.len() + completed.len();
        if held == file_declarations.receipts {
            break;
        }
        let Ok(receipt) = mint(Receipt::for_node(file, id, &node)) else {
            continue;
        };
        if charge.admit(&receipt, held, file_declarations) {
            completed.push(receipt);
        }
    }
    Ok((
        extraction,
        FileReceipts {
            rules,
            declarations,
            completed,
            omitted_declarations,
        },
    ))
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
/// each shared file header once, charged with the first receipt admitted
/// under it. Charged headers are held until the charge is dropped, so a freed
/// header's address can never be reused by another uncharged header. Each
/// held header also charges one map entry; B-tree node overhead beyond that
/// is excluded.
#[derive(Default)]
struct Charge {
    bytes: usize,
    headers: BTreeMap<usize, Arc<MintedHeader>>,
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
        let bytes = receipt.retained_bytes() + shared;
        if self.bytes + bytes > budget.bytes || held == budget.receipts {
            return false;
        }
        self.bytes += bytes;
        self.headers
            .entry(key)
            .or_insert_with(|| Arc::clone(header));
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
    let (mut extraction, facts, receipts, stats) =
        extract_captured_dir_packed_with_policy(root, id, capture, registries, on_file, policy)?;
    (extraction.nodes, extraction.edges) = facts.into_facts().map_err(ExtractError::from)?;
    Ok((extraction, receipts, stats))
}

fn extract_captured_dir_packed_with_policy(
    root: &Path,
    id: &SourceId,
    capture: &Capture,
    registries: &Registries,
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
    let mut omitted_rules = Vec::new();
    let mut stats = IncrementalStats::default();
    source_walk::parallel::map_ordered(
        &paths,
        |path| {
            extract_file_with_measurements(
                capture.file(path)?,
                id,
                registries,
                file_declarations,
                measure_source,
            )
        },
        |path, (mut facts, file_receipts)| {
            on_file(path);
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
            for receipt in file_receipts.completed {
                if staged_completed.admit(&receipt, completed.len(), budget) {
                    completed.push(receipt);
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
            stats.recomputed_files += 1;
            Ok(())
        },
    )?;
    super::complete_directory(&mut extraction, &mut store, root, id)?;
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
    for receipt in completed {
        if matching(&receipt) && charge.admit(&receipt, receipts.len(), budget) {
            receipts.push(receipt);
        }
    }
    drop(matching);
    Ok((extraction, store, receipts, stats))
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
mod tests;
