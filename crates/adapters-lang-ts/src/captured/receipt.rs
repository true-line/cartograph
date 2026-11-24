use super::CapturedError;
mod sizes;
use crate::SourceId;
use core_graph::rules::{DependencyResolution, GuardedExitEvidence, LocalExit};
use core_graph::source::{FactKey, edge_digest, node_digest};
use core_graph::{Edge, Node};
use core_prov::{EvidenceRef, Provenance};
use serde::{Deserialize, Deserializer, Serialize};
pub use sizes::ReceiptSourceSizes;
use source_capture::{CaptureFileRef, CaptureSpanRef, CapturedFile};
use std::collections::BTreeMap;

/// Maximum complete serialized receipt, including identity and all ranges.
pub const MAX_RECEIPT_BYTES: usize = 128 * 1024;
/// Maximum complete range inventory. Receipts never contain partial inventories.
pub const MAX_RECEIPT_RANGES: usize = 1024;
/// Most further captured files one lane receipt may cite besides its primary
/// file (AC-1039).
pub const MAX_RECEIPT_OTHER_FILES: usize = 8;
const MAX_ID_BYTES: usize = 4096;
const MAX_REPO_BYTES: usize = 256;
const RECEIPT_SCHEMA_VERSION: u32 = 3;

/// The source-walk policy version the TypeScript lanes record in an input
/// closure (ADR-0072).
pub const TS_WALK_POLICY: &str = "source-walk-v1";
/// The source-walk policy a directory-walk closure records (ADR-0072 slice
/// 6): the TS/JS selection plus the resolution configs and root
/// `vercel.json` directory completion reads, so a path absent from that
/// enumeration was not there for completion either.
pub const TS_DIRECTORY_WALK_POLICY: &str = "source-walk-v2";
/// The walk policy a Terraform module-walk closure records (ADR-0072 slice
/// 9, AC-1526): the captured `.tf` selection, plus each local module
/// directory the walk lists, which must hold the captured files alone.
pub const TF_MODULE_WALK_POLICY: &str = "tf-module-walk-v1";
/// The walk policy a pipeline reader closure records (ADR-0072 slice 9,
/// AC-1528): the captured selection of workflow files and Dockerfiles,
/// which the reader reads alone.
pub const PIPELINE_WALK_POLICY: &str = "pipeline-walk-v1";
/// The walk policy an environment reader closure records (ADR-0072 slice
/// 9, AC-1530): the captured environment selection, which decides the
/// reader's facts alone when the reader reports them bound to it.
pub const ENVIRONMENT_WALK_POLICY: &str = "environment-walk-v1";
/// The grammar selection rule an input closure records: by file path, as
/// [`Grammar`] selects it.
pub const TS_GRAMMAR_RULE: &str = "grammar-by-path-v1";

/// The input closure of a direct-lexical TypeScript fact of `file`
/// (ADR-0072 slice 5): its retained bytes under contract v4's pins, the
/// registered repository key and the fixed walk and grammar rules. The fact
/// reads no other input; the TypeScript lane takes no layer hints.
pub fn direct_lexical_closure(
    file: &CaptureFileRef,
    repo_key: &str,
) -> Result<super::InputClosureV1, CapturedError> {
    let grammar = Grammar::for_file(file);
    if grammar.provenance_only() {
        return Err(CapturedError::Invalid("receipt contract"));
    }
    let current = contract(4, grammar)?;
    super::InputClosureV1::new(
        current.producer,
        vec![
            current.grammar_package.into(),
            current.parser_package.into(),
        ],
        super::ClosureParameters {
            repo_key: repo_key.into(),
            walk_policy: TS_WALK_POLICY.into(),
            layer_hints: Vec::new(),
            grammar_rule: TS_GRAMMAR_RULE.into(),
        },
        vec![super::ClosureInput::CapturedFile { file: file.clone() }],
    )
}

/// The input closure of a completion-decided TypeScript fact of `file`
/// (ADR-0072 slice 6): its retained bytes, under contract v4's pins and
/// [`TS_DIRECTORY_WALK_POLICY`], plus `walk`, the walk's shared inputs. A
/// closure over more than [`super::MAX_CLOSURE_INPUTS`] inputs is a limit
/// error, never truncated.
pub fn directory_walk_closure(
    file: &CaptureFileRef,
    repo_key: &str,
    mut walk: Vec<super::ClosureInput>,
) -> Result<super::InputClosureV1, CapturedError> {
    let grammar = Grammar::for_file(file);
    if grammar.provenance_only() {
        return Err(CapturedError::Invalid("receipt contract"));
    }
    let current = contract(4, grammar)?;
    walk.push(super::ClosureInput::CapturedFile { file: file.clone() });
    super::InputClosureV1::new(
        current.producer,
        vec![
            current.grammar_package.into(),
            current.parser_package.into(),
        ],
        super::ClosureParameters {
            repo_key: repo_key.into(),
            walk_policy: TS_DIRECTORY_WALK_POLICY.into(),
            layer_hints: Vec::new(),
            grammar_rule: TS_GRAMMAR_RULE.into(),
        },
        walk,
    )
}

/// The input closure of a Terraform fact (ADR-0072 slice 9, AC-1526): the
/// capture's enumeration under [`TF_MODULE_WALK_POLICY`] and the retained
/// bytes of every `.tf` file in `files` the capture holds, under contract
/// v4's pins. Module expansion, policy grants and endpoint closure read
/// across the whole walk, so every fact of the walk shares it. A closure
/// over more than [`super::MAX_CLOSURE_INPUTS`] inputs is a limit error.
pub fn terraform_walk_closure(
    capture: &source_capture::Capture,
    files: &[String],
    repo_key: &str,
) -> Result<super::InputClosureV1, CapturedError> {
    lane_walk_closure(
        Grammar::Terraform,
        TF_MODULE_WALK_POLICY,
        capture,
        files,
        &[],
        repo_key,
    )
}

/// The input closure of a pipeline fact (ADR-0072 slice 9, AC-1528): the
/// capture's enumeration under [`PIPELINE_WALK_POLICY`] and the retained
/// bytes of every pipeline file in `files` the capture holds. The reader
/// reads nothing else over a capture, so every fact it emits shares it.
pub fn pipeline_walk_closure(
    capture: &source_capture::Capture,
    files: &[String],
    repo_key: &str,
) -> Result<super::InputClosureV1, CapturedError> {
    lane_walk_closure(
        Grammar::Pipeline,
        PIPELINE_WALK_POLICY,
        capture,
        files,
        &[],
        repo_key,
    )
}

/// The input closure of an environment fact (ADR-0072 slice 9, AC-1530):
/// the capture's enumeration under [`ENVIRONMENT_WALK_POLICY`] and the
/// retained bytes of every environment file in `files` the capture holds,
/// and of each file of `files` the reader `read` under another grammar (an
/// environment-named TS or config file keeps its own lane, but its bytes
/// are still an input of the reader's facts).
pub fn environment_walk_closure(
    capture: &source_capture::Capture,
    files: &[String],
    read: &[String],
    repo_key: &str,
) -> Result<super::InputClosureV1, CapturedError> {
    lane_walk_closure(
        Grammar::Environment,
        ENVIRONMENT_WALK_POLICY,
        capture,
        files,
        read,
        repo_key,
    )
}

/// A walk closure over `grammar`'s captured files: the capture's
/// enumeration under `walk_policy` and each of `files` of that grammar, or
/// among `also`, the capture holds, under the grammar's contract-v4 pins.
fn lane_walk_closure(
    grammar: Grammar,
    walk_policy: &str,
    capture: &source_capture::Capture,
    files: &[String],
    also: &[String],
    repo_key: &str,
) -> Result<super::InputClosureV1, CapturedError> {
    let current = contract(4, grammar)?;
    let mut inputs = vec![super::ClosureInput::Enumeration {
        source_id: capture.manifest().source_id.clone(),
        capture_id: capture.id().to_string(),
        walk_policy: walk_policy.into(),
    }];
    for path in files {
        if inputs.len() > super::MAX_CLOSURE_INPUTS {
            return Err(CapturedError::Limit("closure inputs"));
        }
        if (Grammar::for_path(path) == grammar || also.contains(path))
            && let Ok(file) = capture.file(path)
        {
            inputs.push(super::ClosureInput::CapturedFile {
                file: file.reference().clone(),
            });
        }
    }
    let mut packages = vec![current.grammar_package.to_string()];
    packages.push(current.parser_package.into());
    super::InputClosureV1::new(
        current.producer,
        packages,
        super::ClosureParameters {
            repo_key: repo_key.into(),
            walk_policy: walk_policy.into(),
            layer_hints: Vec::new(),
            grammar_rule: TS_GRAMMAR_RULE.into(),
        },
        inputs,
    )
}

struct Contract {
    prefix: &'static str,
    producer: &'static str,
    domain: &'static [u8],
    grammar_package: &'static str,
    parser_package: &'static str,
    /// `parser_package` values already-persisted receipts under this contract
    /// may still carry, frozen from before a parser dependency bump changed
    /// the current pin. Newly minted receipts always record `parser_package`;
    /// this list only widens what `validate` still accepts.
    parser_package_legacy: &'static [&'static str],
}

/// The producer contract a receipt of `version` and `grammar` was minted
/// under. Python (AC-1024), Go (AC-1026), Java (AC-1031), Rust (AC-1033),
/// Kotlin (AC-1035) and Terraform (AC-1053) receipts exist in the v3 shape
/// and carry primary provenance alone; each lane's own prefix and domain keep it distinct from
/// every TypeScript receipt. Terraform receipts also exist under v4, v3's
/// compact form with an input closure state (AC-1526).
fn contract(version: u32, grammar: Grammar) -> Result<Contract, CapturedError> {
    match (version, grammar) {
        (4, Grammar::Terraform) => {
            return Ok(Contract {
                prefix: "tf-primary-v2:",
                producer: "t0.adapter-iac/direct-lexical-v2",
                domain: b"cartograph:tf-primary-receipt:v2\0",
                grammar_package: TERRAFORM_PARSER_PACKAGE,
                parser_package: TERRAFORM_PARSER_PACKAGE,
                parser_package_legacy: &[],
            });
        }
        (4, Grammar::Pipeline) => {
            return Ok(Contract {
                prefix: "pipe-primary-v2:",
                producer: "t0.pipeline/declared-v2",
                domain: b"cartograph:pipe-primary-receipt:v2\0",
                grammar_package: YAML_GRAMMAR_PACKAGE,
                parser_package: PARSER_PACKAGE,
                parser_package_legacy: &[],
            });
        }
        (4, Grammar::Environment) => {
            return Ok(Contract {
                prefix: "env-primary-v2:",
                producer: "t0.environments/declared-v2",
                domain: b"cartograph:env-primary-receipt:v2\0",
                grammar_package: YAML_GRAMMAR_PACKAGE,
                parser_package: PARSER_PACKAGE,
                parser_package_legacy: &[],
            });
        }
        _ => {}
    }
    let lane = match grammar {
        Grammar::Python => Some(Contract {
            prefix: "py-primary-v1:",
            producer: "t0.adapter-python/direct-lexical-v1",
            domain: b"cartograph:py-primary-receipt:v1\0",
            grammar_package: PYTHON_GRAMMAR_PACKAGE,
            parser_package: PARSER_PACKAGE,
            parser_package_legacy: &[],
        }),
        Grammar::Go => Some(Contract {
            prefix: "go-primary-v1:",
            producer: "t0.adapter-go/direct-lexical-v1",
            domain: b"cartograph:go-primary-receipt:v1\0",
            grammar_package: GO_GRAMMAR_PACKAGE,
            parser_package: PARSER_PACKAGE,
            parser_package_legacy: &[],
        }),
        Grammar::Java => Some(Contract {
            prefix: "java-primary-v1:",
            producer: "t0.adapter-java/direct-lexical-v1",
            domain: b"cartograph:java-primary-receipt:v1\0",
            grammar_package: JAVA_GRAMMAR_PACKAGE,
            parser_package: PARSER_PACKAGE,
            parser_package_legacy: &[],
        }),
        Grammar::Rust => Some(Contract {
            prefix: "rs-primary-v1:",
            producer: "t0.adapter-rust/direct-lexical-v1",
            domain: b"cartograph:rs-primary-receipt:v1\0",
            grammar_package: RUST_GRAMMAR_PACKAGE,
            parser_package: PARSER_PACKAGE,
            parser_package_legacy: &[],
        }),
        Grammar::Kotlin => Some(Contract {
            prefix: "kt-primary-v1:",
            producer: "t0.adapter-kotlin/direct-lexical-v1",
            domain: b"cartograph:kt-primary-receipt:v1\0",
            grammar_package: KOTLIN_GRAMMAR_PACKAGE,
            parser_package: PARSER_PACKAGE,
            parser_package_legacy: &[],
        }),
        Grammar::Terraform => Some(Contract {
            prefix: "tf-primary-v1:",
            producer: "t0.adapter-iac/direct-lexical-v1",
            domain: b"cartograph:tf-primary-receipt:v1\0",
            grammar_package: TERRAFORM_PARSER_PACKAGE,
            parser_package: TERRAFORM_PARSER_PACKAGE,
            parser_package_legacy: &[],
        }),
        // Workflow files and Dockerfiles, read by the pipeline reader
        // (AC-1528); Dockerfiles are read by the reader's own scanner.
        Grammar::Pipeline => Some(Contract {
            prefix: "pipe-primary-v1:",
            producer: "t0.pipeline/declared-v1",
            domain: b"cartograph:pipe-primary-receipt:v1\0",
            grammar_package: YAML_GRAMMAR_PACKAGE,
            parser_package: PARSER_PACKAGE,
            parser_package_legacy: &[],
        }),
        // Compose, kustomize, Kubernetes, Helm and settings files, read by
        // the environment reader (AC-1530).
        Grammar::Environment => Some(Contract {
            prefix: "env-primary-v1:",
            producer: "t0.environments/declared-v1",
            domain: b"cartograph:env-primary-receipt:v1\0",
            grammar_package: YAML_GRAMMAR_PACKAGE,
            parser_package: PARSER_PACKAGE,
            parser_package_legacy: &[],
        }),
        Grammar::ObservedInput => Some(Contract {
            prefix: "obs-input-v1:",
            producer: "t1.dynamic/observed-span-v1",
            domain: b"cartograph:obs-input-receipt:v1\0",
            grammar_package: OBSERVED_INPUT_FORMAT,
            parser_package: OBSERVED_INPUT_PARSER,
            parser_package_legacy: &[],
        }),
        Grammar::TypeScript | Grammar::Tsx => None,
    };
    if let Some(lane) = lane {
        return match version {
            3 => Ok(lane),
            _ => Err(CapturedError::Invalid("receipt contract version")),
        };
    }
    match version {
        1 => Ok(Contract {
            prefix: "ts-primary-v1:",
            producer: "t0.adapter-ts/direct-lexical-v1",
            domain: b"cartograph:ts-primary-receipt:v1\0",
            // Historical pins are frozen independently of the current producer.
            grammar_package: "tree-sitter-typescript@0.23.2",
            parser_package: "tree-sitter@0.26.12",
            parser_package_legacy: &[],
        }),
        2 => Ok(Contract {
            prefix: "ts-primary-v2:",
            producer: "t0.adapter-ts/direct-lexical-v2",
            domain: b"cartograph:ts-primary-receipt:v2\0",
            grammar_package: GRAMMAR_PACKAGE,
            parser_package: PARSER_PACKAGE,
            // Receipts minted before the tree-sitter 0.27 upgrade recorded
            // this pin under the same v2 contract, and later ones recorded
            // 0.27.0 while 0.27.2 was linked; they must keep validating.
            parser_package_legacy: &["tree-sitter@0.26.12", "tree-sitter@0.27.0"],
        }),
        3 => Ok(Contract {
            prefix: "ts-primary-v3:",
            producer: "t0.adapter-ts/direct-lexical-v3",
            domain: b"cartograph:ts-primary-receipt:v3\0",
            grammar_package: GRAMMAR_PACKAGE,
            parser_package: PARSER_PACKAGE,
            // Receipts minted while the recorded pin lagged the linked
            // 0.27.2 parser (AC-1029) must keep validating.
            parser_package_legacy: &["tree-sitter@0.27.0"],
        }),
        // v3's compact form with an input closure state (ADR-0072 slice 5).
        4 => Ok(Contract {
            prefix: "ts-primary-v4:",
            producer: "t0.adapter-ts/direct-lexical-v4",
            domain: b"cartograph:ts-primary-receipt:v4\0",
            grammar_package: GRAMMAR_PACKAGE,
            parser_package: PARSER_PACKAGE,
            parser_package_legacy: &[],
        }),
        _ => Err(CapturedError::Invalid("receipt contract version")),
    }
}

/// Whether a receipt contract version uses the compact wire form, which
/// derives every range from the header (v3 and later).
fn compact_version(version: u32) -> bool {
    matches!(version, 3 | 4)
}

/// Map a receipt contract version to the rule-evidence schema version it binds.
fn rule_schema_version(version: u32) -> Result<u32, CapturedError> {
    match version {
        1 => Ok(1),
        2..=4 => Ok(2),
        _ => Err(CapturedError::Invalid("receipt contract version")),
    }
}
// Exact dependency pins in Cargo.toml keep these producer inputs truthful.
const GRAMMAR_PACKAGE: &str = "tree-sitter-typescript@0.23.2";
// The parser every adapter links, pinned exactly in each manifest;
// `lane_pins_match_the_adapter_manifests` keeps them in step. Receipts
// minted while this read `tree-sitter@0.27.0` (the code linked 0.27.2) stay
// valid through each contract's legacy list (AC-1029).
const PARSER_PACKAGE: &str = "tree-sitter@0.27.2";
// The Python adapter's exact grammar pin (crates/adapters-lang-python).
const PYTHON_GRAMMAR_PACKAGE: &str = "tree-sitter-python@0.25.0";
// The Go adapter's exact grammar pin (crates/adapters-lang-go).
const GO_GRAMMAR_PACKAGE: &str = "tree-sitter-go@0.25.0";
// The Java adapter's exact grammar pin (crates/adapters-lang-java).
const JAVA_GRAMMAR_PACKAGE: &str = "tree-sitter-java@0.23.5";
// The Rust adapter's exact grammar pin (crates/adapters-lang-rust).
const RUST_GRAMMAR_PACKAGE: &str = "tree-sitter-rust@0.24.2";
// The Kotlin adapter's exact grammar pin (crates/adapters-lang-kotlin).
const KOTLIN_GRAMMAR_PACKAGE: &str = "tree-sitter-kotlin-ng@1.1.0";
// The Terraform adapter's exact HCL parser pin (crates/iac): `hcl-edit` is
// both its grammar and its parser.
const TERRAFORM_PARSER_PACKAGE: &str = "hcl-edit@0.9.8";
// The pipeline reader's exact YAML grammar pin (crates/ingest).
const YAML_GRAMMAR_PACKAGE: &str = "tree-sitter-yaml@0.7.2";
// Observation inputs are JSON documents (state, Pulumi) or JSON lines
// (OTLP), cited by the byte spans the T1 producers record (ADR-0079).
const OBSERVED_INPUT_FORMAT: &str = "json/jsonl";
// The parser the T1 producers read them with.
const OBSERVED_INPUT_PARSER: &str = "serde_json@1";

/// The capture identity prefix of manifest observation inputs (ADR-0080).
const INPUT_SOURCE_PREFIX: &str = "input/";

/// Whether `source` is a registered source's observation input identity,
/// `input/src_<32 hex>`.
pub fn is_input_source(source: &source_capture::SourceId) -> bool {
    source
        .as_str()
        .strip_prefix(INPUT_SOURCE_PREFIX)
        .is_some_and(registered_source_id)
}

fn registered_source_id(id: &str) -> bool {
    id.strip_prefix("src_").is_some_and(|value| {
        value.len() == 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Actual grammar selected by the ordinary parser for the retained path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grammar {
    /// Bare .ts files permit TypeScript angle-bracket casts.
    #[serde(rename = "typescript")]
    TypeScript,
    /// Other supported JS/TS extensions use the JSX-aware grammar.
    Tsx,
    /// `.py` files, parsed by the Python adapter (AC-1024).
    Python,
    /// `.go` files, parsed by the Go adapter (AC-1026).
    Go,
    /// `.java` files, parsed by the Java adapter (AC-1031).
    Java,
    /// `.rs` files, parsed by the Rust adapter (AC-1033).
    Rust,
    /// `.kt` and `.kts` files, parsed by the Kotlin adapter (AC-1035).
    Kotlin,
    /// `.tf` files, parsed by the Terraform adapter (AC-1053).
    Terraform,
    /// Workflow files and Dockerfiles, as the pipeline reader selects them,
    /// read by it (AC-1528).
    Pipeline,
    /// Files the environment reader selects that no language or pipeline
    /// grammar claims, read by it (AC-1530).
    Environment,
    /// A manifest observation input (Terraform state, Pulumi export, OTLP
    /// trace) captured under `input/<source id>`, read by the T1 producers
    /// (ADR-0079, AC-1045). Chosen by the capture identity, not the path.
    ObservedInput,
}

impl Grammar {
    /// A non-TypeScript lane whose receipts bind primary provenance alone.
    pub(super) fn provenance_only(self) -> bool {
        matches!(
            self,
            Self::Python
                | Self::Go
                | Self::Java
                | Self::Rust
                | Self::Kotlin
                | Self::Terraform
                | Self::Pipeline
                | Self::Environment
                | Self::ObservedInput
        )
    }

    /// A grammar whose receipts have contract v4, with an input closure
    /// state: TypeScript (slice 5), Terraform (slice 9, AC-1526) and
    /// pipelines (AC-1528) and environments (AC-1530).
    pub(super) fn has_v4(self) -> bool {
        !self.provenance_only()
            || matches!(self, Self::Terraform | Self::Pipeline | Self::Environment)
    }

    /// The grammar a receipt citing `file` is minted under: an observation
    /// input capture's, else the one its path selects.
    pub(super) fn for_file(file: &CaptureFileRef) -> Self {
        if is_input_source(&file.source_id) {
            Self::ObservedInput
        } else {
            Self::for_path(&file.path)
        }
    }

    pub(super) fn for_path(path: &str) -> Self {
        if path.ends_with(".ts") {
            Self::TypeScript
        } else if path.ends_with(".py") {
            Self::Python
        } else if path.ends_with(".go") {
            Self::Go
        } else if path.ends_with(".java") {
            Self::Java
        } else if path.ends_with(".rs") {
            Self::Rust
        } else if path.ends_with(".kt") || path.ends_with(".kts") {
            Self::Kotlin
        } else if path.ends_with(".tf") {
            Self::Terraform
        } else if ingest::pipelines::is_pipeline_file(path) {
            Self::Pipeline
        } else if environment_path(path) {
            Self::Environment
        } else {
            Self::Tsx
        }
    }
}

/// Position in the producer's fixed structural range inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeRole {
    /// Primary provenance evidence, in stored provenance order.
    Provenance,
    /// Guarded return/throw source.
    RuleExit,
    /// Enclosing branch statement, indexed by condition.
    ConditionBranch,
    /// Branch expression, indexed by condition.
    ConditionExpression,
    /// Optional return value, index zero.
    ReturnValue,
    /// Throw value, index zero.
    ThrowValue,
    /// Dependency occurrence, indexed by dependency.
    Dependency,
    /// A binding's declaration, indexed by its dependency.
    DependencyDeclaration,
    /// Redacted original source, indexed by redaction.
    Redaction,
    /// Local declaration, indexed by definition (v2 only).
    DefinitionDeclaration,
    /// Original admitted use, using a global lexical per-role counter (v2 only).
    DefinitionUse,
    /// Original initializer, indexed by definition (v2 only).
    DefinitionInitializer,
    /// Arena expression, using a global producer-order counter (v2 only).
    DefinitionExpression,
    /// Initializer prerequisite, using a global per-role counter (v2 only).
    DefinitionDependency,
    /// Binding declaration paired with its initializer dependency index (v2 only).
    DefinitionDependencyDeclaration,
}

/// One unchanged original citation and its exact retained raw-byte binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptRange {
    /// Structural source role, never source text.
    pub role: RangeRole,
    /// Index within the role's original container; never a caller-chosen offset.
    pub index: u32,
    /// Original legacy reference, preserved without changing its wire contract.
    #[serde(deserialize_with = "strict_evidence")]
    pub evidence: EvidenceRef,
    /// Same offsets into the same immutable captured file.
    pub captured: CaptureSpanRef,
}

fn strict_evidence<'de, D: Deserializer<'de>>(deserializer: D) -> Result<EvidenceRef, D::Error> {
    // The legacy EvidenceRef decoder is permissive. The new receipt envelope
    // permits exactly its unchanged five fields, so ignored source-bearing
    // fields cannot survive as an alternate wire representation of a receipt.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct WireEvidence {
        repo: String,
        path: String,
        byte_start: u64,
        byte_end: u64,
        commit_sha: String,
    }
    let wire = WireEvidence::deserialize(deserializer)?;
    Ok(EvidenceRef {
        repo: wire.repo,
        path: wire.path,
        byte_start: wire.byte_start,
        byte_end: wire.byte_end,
        commit_sha: wire.commit_sha,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceScope {
    PrimarySourceOnly,
}

/// The closure state a receipt records: contracts v1 to v3 carry the bare
/// legacy string, v4 a [`ReceiptClosure`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum InputClosure {
    Legacy(LegacyClosure),
    Closure(ReceiptClosure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyClosure {
    InputClosureNotEstablished,
}

impl InputClosure {
    const LEGACY: Self = Self::Legacy(LegacyClosure::InputClosureNotEstablished);
}

/// Whether a v4 receipt's fact has an established input closure
/// (ADR-0072 decision 1). An established closure is not business truth and
/// never changes tier or confidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReceiptClosure {
    /// Not established, for these sorted, unique and nonempty reasons.
    InputClosureNotEstablished {
        /// Why not.
        reasons: Vec<super::ClosureGapReason>,
    },
    /// The fact is a function of exactly the inputs this closure binds.
    Established {
        /// The persisted [`super::InputClosureV1`] id.
        closure_id: String,
    },
}

impl ReceiptClosure {
    fn valid(&self) -> bool {
        match self {
            Self::InputClosureNotEstablished { reasons } => {
                !reasons.is_empty() && reasons.windows(2).all(|pair| pair[0] < pair[1])
            }
            Self::Established { closure_id } => super::is_closure_id(closure_id),
        }
    }
}

/// Compact v3 wire range.
///
/// There is no explicit/full range form in v3: `validate()` already requires
/// every range to be same-file, same-repo, and same-commit with equal evidence
/// and capture offsets, so every valid range is derivable. An explicit form
/// could therefore never be canonical.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactRange {
    role: RangeRole,
    index: u32,
    byte_start: u64,
    byte_end: u64,
    /// Position in `other_files` of the file a range cites when it is not the
    /// primary file (AC-1039); absent for the primary file, never `null`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_index"
    )]
    file: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Content {
    schema_version: u32,
    source_id: source_capture::SourceId,
    repo_key: String,
    file: CaptureFileRef,
    commit_sha: Option<String>,
    producer_contract: String,
    grammar: Grammar,
    grammar_package: String,
    parser_package: String,
    fact_key: FactKey,
    fact_digest: String,
    ranges: Vec<ReceiptRange>,
    /// Further captured files a lane receipt cites, sorted by path (AC-1039).
    other_files: Vec<CaptureFileRef>,
    source_sizes: Option<ReceiptSourceSizes>,
    source_scope: SourceScope,
    input_closure: InputClosure,
}

#[derive(Serialize)]
struct WireContentFull<'a> {
    schema_version: u32,
    source_id: &'a source_capture::SourceId,
    repo_key: &'a str,
    file: &'a CaptureFileRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_sha: Option<&'a str>,
    producer_contract: &'a str,
    grammar: Grammar,
    grammar_package: &'a str,
    parser_package: &'a str,
    fact_key: &'a FactKey,
    fact_digest: &'a str,
    ranges: &'a [ReceiptRange],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    other_files: &'a [CaptureFileRef],
    #[serde(skip_serializing_if = "Option::is_none")]
    source_sizes: Option<&'a ReceiptSourceSizes>,
    source_scope: SourceScope,
    input_closure: &'a InputClosure,
}

#[derive(Serialize)]
struct WireContentCompact<'a> {
    schema_version: u32,
    source_id: &'a source_capture::SourceId,
    repo_key: &'a str,
    file: &'a CaptureFileRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_sha: Option<&'a str>,
    producer_contract: &'a str,
    grammar: Grammar,
    grammar_package: &'a str,
    parser_package: &'a str,
    fact_key: &'a FactKey,
    fact_digest: &'a str,
    ranges: Vec<CompactRange>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    other_files: &'a [CaptureFileRef],
    #[serde(skip_serializing_if = "Option::is_none")]
    source_sizes: Option<&'a ReceiptSourceSizes>,
    source_scope: SourceScope,
    input_closure: &'a InputClosure,
}

/// One decoded wire range: the v1/v2 full form or the v3 compact form. Each
/// variant denies unknown fields, so an object matches at most one of them.
#[derive(Deserialize)]
#[serde(untagged)]
enum WireRange {
    Full(ReceiptRange),
    Compact(CompactRange),
}

/// Strict wire decoder for every contract version. Derived struct decoding
/// keeps serde's duplicate-field and unknown-field rejection, so no receipt
/// gains an alternate wire representation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireContent {
    schema_version: u32,
    source_id: source_capture::SourceId,
    repo_key: String,
    file: CaptureFileRef,
    #[serde(default, deserialize_with = "present_string")]
    commit_sha: Option<String>,
    producer_contract: String,
    grammar: Grammar,
    grammar_package: String,
    parser_package: String,
    fact_key: FactKey,
    fact_digest: String,
    ranges: Vec<WireRange>,
    #[serde(default, deserialize_with = "present_files")]
    other_files: Vec<CaptureFileRef>,
    #[serde(default, deserialize_with = "sizes::present")]
    source_sizes: Option<ReceiptSourceSizes>,
    source_scope: SourceScope,
    input_closure: InputClosure,
}

/// An optional field that, when present, must be a string: an explicit `null`
/// would otherwise be a second encoding of an absent `commit_sha`.
fn present_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

/// A present `other_files` list must be non-empty and bounded: an empty one
/// would be a second encoding of an absent list.
fn present_files<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<CaptureFileRef>, D::Error> {
    struct Bounded;
    impl<'de> serde::de::Visitor<'de> for Bounded {
        type Value = Vec<CaptureFileRef>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a bounded non-empty list of captured files")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut files = Vec::new();
            while let Some(file) = seq.next_element()? {
                if files.len() == MAX_RECEIPT_OTHER_FILES {
                    return Err(serde::de::Error::custom("receipt files exceed limit"));
                }
                files.push(file);
            }
            if files.is_empty() {
                return Err(serde::de::Error::custom("empty receipt file list"));
            }
            Ok(files)
        }
    }
    deserializer.deserialize_seq(Bounded)
}

/// A present range file index must be a number, never `null`.
fn present_index<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u32>, D::Error> {
    u32::deserialize(deserializer).map(Some)
}

#[derive(Serialize)]
struct ExpandedReceipt<'a> {
    receipt_id: &'a str,
    content: WireContentFull<'a>,
}

impl Content {
    fn as_full_wire<'a>(&'a self, commit_sha: Option<&'a str>) -> WireContentFull<'a> {
        WireContentFull {
            schema_version: self.schema_version,
            source_id: &self.source_id,
            repo_key: &self.repo_key,
            file: &self.file,
            commit_sha,
            producer_contract: &self.producer_contract,
            grammar: self.grammar,
            grammar_package: &self.grammar_package,
            parser_package: &self.parser_package,
            fact_key: &self.fact_key,
            fact_digest: &self.fact_digest,
            ranges: &self.ranges,
            other_files: &self.other_files,
            source_sizes: self.source_sizes.as_ref(),
            source_scope: self.source_scope,
            input_closure: &self.input_closure,
        }
    }
}

impl Serialize for Content {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if compact_version(self.schema_version) {
            let ranges = self
                .ranges
                .iter()
                .map(|range| CompactRange {
                    role: range.role,
                    index: range.index,
                    byte_start: range.evidence.byte_start,
                    byte_end: range.evidence.byte_end,
                    // A range citing a file outside the list is caught by
                    // `validate()`; it serializes as the primary file.
                    file: (range.captured.file != self.file)
                        .then(|| {
                            self.other_files
                                .iter()
                                .position(|file| *file == range.captured.file)
                        })
                        .flatten()
                        .and_then(|index| u32::try_from(index).ok()),
                })
                .collect();
            WireContentCompact {
                schema_version: self.schema_version,
                source_id: &self.source_id,
                repo_key: &self.repo_key,
                file: &self.file,
                commit_sha: self.commit_sha.as_deref(),
                producer_contract: &self.producer_contract,
                grammar: self.grammar,
                grammar_package: &self.grammar_package,
                parser_package: &self.parser_package,
                fact_key: &self.fact_key,
                fact_digest: &self.fact_digest,
                ranges,
                other_files: &self.other_files,
                source_sizes: self.source_sizes.as_ref(),
                source_scope: self.source_scope,
                input_closure: &self.input_closure,
            }
            .serialize(serializer)
        } else {
            self.as_full_wire(self.commit_sha.as_deref())
                .serialize(serializer)
        }
    }
}

/// Exact serialized size of the v2-shaped ranges a v3 range list expands to,
/// computed without cloning the shared identity into each range. It is a lower
/// bound on the expanded receipt that `validate()` measures, so preflighting
/// it never rejects a receipt `validate()` would accept, while a near-limit
/// compact payload can't allocate its expansion first. `None` if not a v3
/// compact list or the arithmetic overflows.
fn expanded_range_bytes(
    ranges: &[WireRange],
    repo_key: &str,
    file: &CaptureFileRef,
    other_files: &[CaptureFileRef],
    commit_sha: &str,
) -> Option<usize> {
    let digits = |value: u64| value.checked_ilog10().map_or(1, |log| log as usize + 1);
    let provenance = serde_json::to_vec(&RangeRole::Provenance).ok()?.len();
    // One template range per cited file, with every variable part
    // zero-length or "0"; the primary file first.
    let fixed = std::iter::once(file)
        .chain(other_files)
        .map(|file| {
            let template = serde_json::to_vec(&ReceiptRange {
                role: RangeRole::Provenance,
                index: 0,
                evidence: EvidenceRef {
                    repo: repo_key.to_owned(),
                    path: file.path.clone(),
                    byte_start: 0,
                    byte_end: 0,
                    commit_sha: commit_sha.to_owned(),
                },
                captured: CaptureSpanRef {
                    file: file.clone(),
                    byte_start: 0,
                    byte_end: 0,
                },
            })
            .ok()?
            .len();
            template.checked_sub(provenance + 5)
        })
        .collect::<Option<Vec<_>>>()?;
    let mut total = ranges.len().saturating_sub(1) + 2;
    for range in ranges {
        let WireRange::Compact(range) = range else {
            return None;
        };
        let role = serde_json::to_vec(&range.role).ok()?.len();
        let slot = match range.file {
            None => 0,
            Some(index) => usize::try_from(index).ok()?.checked_add(1)?,
        };
        total = total
            .checked_add(*fixed.get(slot)?)?
            .checked_add(role)?
            .checked_add(digits(u64::from(range.index)))?
            .checked_add(2 * digits(range.byte_start))?
            .checked_add(2 * digits(range.byte_end))?;
    }
    Some(total)
}

impl<'de> Deserialize<'de> for Content {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = WireContent::deserialize(deserializer)?;
        if wire.ranges.len() > MAX_RECEIPT_RANGES {
            return Err(serde::de::Error::custom("receipt ranges exceed limit"));
        }
        let ranges = match (wire.schema_version, &wire.commit_sha) {
            (1 | 2, None) => wire
                .ranges
                .into_iter()
                .map(|range| match range {
                    WireRange::Full(range) => Ok(range),
                    WireRange::Compact(_) => Err(()),
                })
                .collect::<Result<Vec<_>, ()>>(),
            (3 | 4, Some(commit_sha))
                if expanded_range_bytes(
                    &wire.ranges,
                    &wire.repo_key,
                    &wire.file,
                    &wire.other_files,
                    commit_sha,
                )
                .is_none_or(|bytes| bytes > MAX_RECEIPT_BYTES) =>
            {
                return Err(serde::de::Error::custom("receipt expansion exceeds limit"));
            }
            (3 | 4, Some(commit_sha)) => wire
                .ranges
                .into_iter()
                .map(|range| match range {
                    // `expanded_range_bytes` admitted these clones above,
                    // and every range's file index is in bounds.
                    WireRange::Compact(range) => {
                        let file = match range.file {
                            None => &wire.file,
                            Some(index) => &wire.other_files[index as usize],
                        };
                        Ok(ReceiptRange {
                            role: range.role,
                            index: range.index,
                            evidence: EvidenceRef {
                                repo: wire.repo_key.clone(),
                                path: file.path.clone(),
                                byte_start: range.byte_start,
                                byte_end: range.byte_end,
                                commit_sha: commit_sha.clone(),
                            },
                            captured: CaptureSpanRef {
                                file: file.clone(),
                                byte_start: range.byte_start,
                                byte_end: range.byte_end,
                            },
                        })
                    }
                    WireRange::Full(_) => Err(()),
                })
                .collect::<Result<Vec<_>, ()>>(),
            _ => Err(()),
        }
        .map_err(|()| serde::de::Error::custom("receipt range form does not match its version"))?;
        // v4 keeps v3's file binding; `validate` admits it for TS alone.
        if !matches!(wire.schema_version, 3 | 4) && !wire.other_files.is_empty() {
            return Err(serde::de::Error::custom(
                "receipt files do not match its version",
            ));
        }
        Ok(Self {
            schema_version: wire.schema_version,
            source_id: wire.source_id,
            repo_key: wire.repo_key,
            file: wire.file,
            commit_sha: wire.commit_sha,
            producer_contract: wire.producer_contract,
            grammar: wire.grammar,
            grammar_package: wire.grammar_package,
            parser_package: wire.parser_package,
            fact_key: wire.fact_key,
            fact_digest: wire.fact_digest,
            ranges,
            other_files: wire.other_files,
            source_sizes: wire.source_sizes,
            source_scope: wire.source_scope,
            input_closure: wire.input_closure,
        })
    }
}

/// Immutable metadata-only producer receipt, separate from graph properties.
/// Public decoding/validation establishes internal content consistency only;
/// it cannot prove that caller-supplied JSON came from this producer invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Receipt {
    receipt_id: String,
    content: Content,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReceipt {
    receipt_id: String,
    content: Content,
}

impl<'de> Deserialize<'de> for Receipt {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = WireReceipt::deserialize(deserializer)
            .map_err(|_| serde::de::Error::custom("invalid primary source receipt metadata"))?;
        let receipt = Self {
            receipt_id: wire.receipt_id,
            content: wire.content,
        };
        receipt.validate().map_err(serde::de::Error::custom)?;
        Ok(receipt)
    }
}

fn bounded_identity(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn hash_has_prefix(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

pub(super) fn validate_source_repo(
    source: &source_capture::SourceId,
    repo: &str,
) -> Result<(), CapturedError> {
    let id = source.as_str();
    // An observation input's receipts live in its own namespace: the input
    // identity is both its source and its repo (ADR-0079).
    let input = is_input_source(source);
    if !(registered_source_id(id) || input)
        || !bounded_identity(repo, MAX_REPO_BYTES)
        || repo.strip_prefix("local/").is_some_and(|local| local != id)
        || (input && repo != id)
        || (!input && repo.starts_with(INPUT_SOURCE_PREFIX))
    {
        return Err(CapturedError::Invalid("registered source identity"));
    }
    Ok(())
}

pub(super) fn validate_identity(file: &CaptureFileRef, repo: &str) -> Result<(), CapturedError> {
    validate_file(file, repo, false)
}

/// [`validate_identity`]; with `config`, a TS resolution config the walk
/// captured (a `package.json`, a tsconfig) is a valid path too (AC-1522).
fn validate_file(file: &CaptureFileRef, repo: &str, config: bool) -> Result<(), CapturedError> {
    validate_source_repo(&file.source_id, repo)?;
    // Capture IDs are versioned hashes; CaptureStore independently verifies
    // membership and the actual raw object before a source read.
    if !hash_has_prefix(&file.capture_id, "capture-v1:")
        || !hash_has_prefix(&file.digest, "")
        || file.byte_len > source_capture::MAX_FILE_BYTES
        || !super::valid_path(&file.path)
        || !(is_input_source(&file.source_id)
            || super::supported(&file.path)
            || (config && super::is_config_path(&file.path))
            || file.path.ends_with(".py")
            || file.path.ends_with(".go")
            || file.path.ends_with(".java")
            || file.path.ends_with(".rs")
            || file.path.ends_with(".kt")
            || file.path.ends_with(".kts")
            || file.path.ends_with(".tf")
            || ingest::pipelines::is_pipeline_file(&file.path)
            || environment_path(&file.path))
    {
        return Err(CapturedError::Invalid("capture file binding"));
    }
    Ok(())
}

/// A lane receipt may cite further captured files besides its primary file
/// (AC-1039): each of the same capture and grammar, listed once in path order,
/// cited by at least one range, with the primary file cited first. A
/// TypeScript receipt whose ranges are all provenance may too, citing other
/// TS/JS sources of its capture, such as a barrel (AC-1520), or its
/// resolution configs, such as a `package.json` (AC-1522); a rule's
/// inventory and measured receipts stay single-file.
fn validate_other_files(content: &Content) -> Result<(), CapturedError> {
    if content.other_files.is_empty() {
        return Ok(());
    }
    let typescript = !content.grammar.provenance_only();
    // Contract v4 (TypeScript, Terraform) keeps the v3 file binding.
    let schema =
        content.schema_version == 3 || (content.grammar.has_v4() && content.schema_version == 4);
    if !schema
        || (typescript
            && content
                .ranges
                .iter()
                .any(|range| range.role != RangeRole::Provenance))
        || content.source_sizes.is_some()
        || content.other_files.len() > MAX_RECEIPT_OTHER_FILES
        || content
            .ranges
            .first()
            .is_none_or(|range| range.captured.file != content.file)
    {
        return Err(CapturedError::Invalid("receipt files"));
    }
    let mut previous: Option<&str> = None;
    for file in &content.other_files {
        // A TS receipt may cite the walk's resolution configs (AC-1522).
        validate_file(file, &content.repo_key, typescript)?;
        if file.source_id != content.file.source_id
            || file.capture_id != content.file.capture_id
            || file.path == content.file.path
            || !same_lane(Grammar::for_file(file), content.grammar)
            || previous.is_some_and(|previous| previous >= file.path.as_str())
            || !content
                .ranges
                .iter()
                .any(|range| range.captured.file == *file)
        {
            return Err(CapturedError::Invalid("receipt files"));
        }
        previous = Some(&file.path);
    }
    Ok(())
}

/// Whether a file of `other` grammar may be cited by a receipt of
/// `primary`: the same lane grammar, or either TS grammar for a TypeScript
/// receipt (a `.tsx` component may re-export a `.ts` module; a config's path
/// selects the TSX grammar).
fn same_lane(other: Grammar, primary: Grammar) -> bool {
    other == primary
        || (!primary.provenance_only() && matches!(other, Grammar::TypeScript | Grammar::Tsx))
}

fn canonical(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, canonical(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(canonical).collect())
        }
        other => other,
    }
}

fn content_id(content: &Content) -> Result<String, CapturedError> {
    let value = serde_json::to_value(content)
        .map_err(|_| CapturedError::Invalid("receipt serialization"))?;
    let bytes = serde_json::to_vec(&canonical(value))
        .map_err(|_| CapturedError::Invalid("receipt serialization"))?;
    let contract = contract(content.schema_version, content.grammar)?;
    let mut input = contract.domain.to_vec();
    input.extend(bytes);
    Ok(format!(
        "{}{}",
        contract.prefix,
        core_prov::content_hash(&input)
    ))
}

fn inventory(
    props: &serde_json::Value,
    version: u32,
    grammar: Grammar,
) -> Result<Vec<(RangeRole, u32, EvidenceRef)>, CapturedError> {
    contract(version, grammar)?;
    // An observed receipt cites the observation a T1 producer recorded beside
    // the fact's own provenance; a fact the observation created (a resolved
    // placeholder, a filled channel) carries it as its provenance (ADR-0079).
    let key = match grammar {
        Grammar::ObservedInput if props.get("observed_prov").is_some() => "observed_prov",
        _ => "prov",
    };
    let provenance: Provenance = serde_json::from_value(
        props
            .get(key)
            .cloned()
            .ok_or(CapturedError::Invalid("missing provenance"))?,
    )
    .map_err(|_| CapturedError::Invalid("malformed provenance"))?;
    provenance
        .validate()
        .map_err(|_| CapturedError::Invalid("invalid provenance"))?;
    let mut ranges = Vec::new();
    let mut push = |role, index: usize, evidence: &EvidenceRef| -> Result<(), CapturedError> {
        if ranges.len() == MAX_RECEIPT_RANGES {
            return Err(CapturedError::Limit("receipt ranges"));
        }
        ranges.push((
            role,
            u32::try_from(index).map_err(|_| CapturedError::Limit("range index"))?,
            evidence.clone(),
        ));
        Ok(())
    };
    for (index, evidence) in provenance.evidence.iter().enumerate() {
        push(RangeRole::Provenance, index, evidence)?;
    }
    if grammar == Grammar::ObservedInput {
        return Ok(ranges);
    }
    if let Some(value) = props.get("rule") {
        // The non-TS lanes carry primary provenance alone.
        if grammar.provenance_only() {
            return Err(CapturedError::Invalid("rule source inventory"));
        }
        let rule = GuardedExitEvidence::from_value(value.clone())
            .map_err(|_| CapturedError::Invalid("rule source inventory"))?;
        if rule.schema_version != rule_schema_version(version)? {
            return Err(CapturedError::Invalid("rule and receipt version"));
        }
        push(RangeRole::RuleExit, 0, &rule.exit_source)?;
        for (index, condition) in rule.conditions.iter().enumerate() {
            push(RangeRole::ConditionBranch, index, &condition.branch_source)?;
            push(
                RangeRole::ConditionExpression,
                index,
                &condition.expression.source,
            )?;
        }
        match &rule.effect {
            LocalExit::Return { value: Some(value) } => {
                push(RangeRole::ReturnValue, 0, &value.source)?
            }
            LocalExit::Return { value: None } => {}
            LocalExit::Throw { value } => push(RangeRole::ThrowValue, 0, &value.source)?,
            // The TypeScript producer records exits only; a captured receipt
            // never covers a guarded effect (#1998).
            LocalExit::Assign { .. } | LocalExit::Call { .. } => {
                return Err(CapturedError::Invalid("rule source inventory"));
            }
        }
        for (index, dependency) in rule.dependencies.iter().enumerate() {
            push(RangeRole::Dependency, index, &dependency.source)?;
            if let DependencyResolution::Binding { declaration, .. } = &dependency.resolution {
                push(RangeRole::DependencyDeclaration, index, declaration)?;
            }
        }
        if let Some(definitions) = &rule.local_definitions {
            let mut use_index = 0;
            let mut expression_index = 0;
            let mut dependency_index = 0;
            for (index, definition) in definitions.iter().enumerate() {
                push(
                    RangeRole::DefinitionDeclaration,
                    index,
                    &definition.declaration,
                )?;
                for usage in &definition.uses {
                    push(RangeRole::DefinitionUse, use_index, usage)?;
                    use_index += 1;
                }
                push(
                    RangeRole::DefinitionInitializer,
                    index,
                    &definition.initializer.source,
                )?;
                for node in &definition.expression.nodes {
                    push(
                        RangeRole::DefinitionExpression,
                        expression_index,
                        &node.expression.source,
                    )?;
                    expression_index += 1;
                }
                for dependency in &definition.dependencies {
                    push(
                        RangeRole::DefinitionDependency,
                        dependency_index,
                        &dependency.source,
                    )?;
                    if let DependencyResolution::Binding { declaration, .. } =
                        &dependency.resolution
                    {
                        push(
                            RangeRole::DefinitionDependencyDeclaration,
                            dependency_index,
                            declaration,
                        )?;
                    }
                    dependency_index += 1;
                }
            }
        }
        for (index, redaction) in rule.redactions.iter().enumerate() {
            push(RangeRole::Redaction, index, &redaction.source)?;
        }
    }
    Ok(ranges)
}

fn valid_inventory_shape(ranges: &[ReceiptRange], version: u32) -> bool {
    let mut next = 0;
    let mut take = |role, index| {
        if ranges
            .get(next)
            .is_some_and(|range| range.role == role && range.index == index)
        {
            next += 1;
            true
        } else {
            false
        }
    };
    let mut index = 0;
    while take(RangeRole::Provenance, index) {
        index += 1;
    }
    if index == 0 {
        return false;
    }
    // Owner, declaration and relationship receipts contain primary provenance alone. Rule
    // inventories add the fixed structural traversal without grouping away
    // duplicate source references or substituting arbitrary JSON pointers.
    if take(RangeRole::RuleExit, 0) {
        index = 0;
        while take(RangeRole::ConditionBranch, index) {
            if !take(RangeRole::ConditionExpression, index) {
                return false;
            }
            index += 1;
        }
        if !take(RangeRole::ReturnValue, 0) {
            take(RangeRole::ThrowValue, 0);
        }
        index = 0;
        while take(RangeRole::Dependency, index) {
            take(RangeRole::DependencyDeclaration, index);
            index += 1;
        }
        if matches!(version, 2..=4) {
            let mut definition = 0;
            let mut usage = 0;
            let mut expression = 0;
            let mut dependency = 0;
            while take(RangeRole::DefinitionDeclaration, definition) {
                let previous_uses = usage;
                while take(RangeRole::DefinitionUse, usage) {
                    usage += 1;
                }
                if usage == previous_uses || !take(RangeRole::DefinitionInitializer, definition) {
                    return false;
                }
                let previous_expressions = expression;
                while take(RangeRole::DefinitionExpression, expression) {
                    expression += 1;
                }
                if expression == previous_expressions {
                    return false;
                }
                while take(RangeRole::DefinitionDependency, dependency) {
                    take(RangeRole::DefinitionDependencyDeclaration, dependency);
                    dependency += 1;
                }
                definition += 1;
            }
            if definition as usize > core_graph::rules::MAX_LOCAL_DEFINITIONS
                || expression as usize > core_graph::rules::MAX_DEFINITION_NODES
                || dependency as usize > core_graph::rules::MAX_INITIALIZER_DEPENDENCIES
            {
                return false;
            }
        }
        index = 0;
        while take(RangeRole::Redaction, index) {
            index += 1;
        }
    }
    next == ranges.len()
}

impl Receipt {
    /// Mint against `file`. With `capture`, a lane fact whose evidence also
    /// cites other files of that capture binds each range to its own file
    /// (AC-1039); without one, every range must lie in `file`.
    fn mint(
        file: &CapturedFile,
        capture: Option<&source_capture::Capture>,
        id: &SourceId,
        fact_key: FactKey,
        fact_digest: String,
        props: &serde_json::Value,
    ) -> Result<Self, CapturedError> {
        let grammar = Grammar::for_file(file.reference());
        let current = contract(RECEIPT_SCHEMA_VERSION, grammar)?;
        let mut other_files = BTreeMap::new();
        let ranges = inventory(props, RECEIPT_SCHEMA_VERSION, grammar)?
            .into_iter()
            .map(|(role, index, evidence)| {
                let cited = match capture {
                    Some(capture) if evidence.path != file.reference().path => {
                        let other = capture
                            .file(&evidence.path)
                            .map_err(|_| CapturedError::Invalid("receipt files"))?;
                        other_files.insert(evidence.path.clone(), other.reference().clone());
                        other
                    }
                    _ => file,
                };
                let captured = cited.span(evidence.byte_start, evidence.byte_end)?;
                Ok(ReceiptRange {
                    role,
                    index,
                    evidence,
                    captured,
                })
            })
            .collect::<Result<Vec<_>, CapturedError>>()?;
        let content = Content {
            schema_version: RECEIPT_SCHEMA_VERSION,
            source_id: file.reference().source_id.clone(),
            repo_key: id.repo.into(),
            file: file.reference().clone(),
            commit_sha: (RECEIPT_SCHEMA_VERSION == 3).then(|| id.commit.into()),
            producer_contract: current.producer.into(),
            grammar,
            grammar_package: current.grammar_package.into(),
            parser_package: current.parser_package.into(),
            fact_key,
            fact_digest,
            ranges,
            other_files: other_files.into_values().collect(),
            source_sizes: None,
            source_scope: SourceScope::PrimarySourceOnly,
            input_closure: InputClosure::LEGACY,
        };
        let receipt = Self {
            receipt_id: content_id(&content)?,
            content,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub(super) fn for_node(
        file: &CapturedFile,
        id: &SourceId,
        node: &Node,
    ) -> Result<Self, CapturedError> {
        Self::for_node_in(file, None, id, node)
    }

    pub(super) fn for_node_in(
        file: &CapturedFile,
        capture: Option<&source_capture::Capture>,
        id: &SourceId,
        node: &Node,
    ) -> Result<Self, CapturedError> {
        Self::mint(
            file,
            capture,
            id,
            FactKey::Node {
                id: node.id.clone(),
            },
            node_digest(node).map_err(|_| CapturedError::Invalid("fact digest"))?,
            &node.props,
        )
    }

    pub(super) fn for_edge(
        file: &CapturedFile,
        id: &SourceId,
        edge: &Edge,
    ) -> Result<Self, CapturedError> {
        Self::for_edge_in(file, None, id, edge)
    }

    pub(super) fn for_edge_in(
        file: &CapturedFile,
        capture: Option<&source_capture::Capture>,
        id: &SourceId,
        edge: &Edge,
    ) -> Result<Self, CapturedError> {
        Self::mint(
            file,
            capture,
            id,
            FactKey::Edge {
                source: edge.src.clone(),
                label: edge.label.clone(),
                destination: edge.dst.clone(),
            },
            edge_digest(edge).map_err(|_| CapturedError::Invalid("fact digest"))?,
            &edge.props,
        )
    }

    /// Immutable receipt identity, distinct from the complete emitted fact hash.
    pub fn id(&self) -> &str {
        &self.receipt_id
    }
    /// Registered logical source; not physical directory continuity.
    pub fn source_id(&self) -> &source_capture::SourceId {
        &self.content.source_id
    }
    /// Exact registered graph namespace.
    pub fn repo_key(&self) -> &str {
        &self.content.repo_key
    }
    /// Full primary-file capture binding.
    pub fn file(&self) -> &CaptureFileRef {
        &self.content.file
    }
    /// Further captured files a lane receipt's ranges cite, in path order
    /// (AC-1039); empty for a single-file receipt.
    pub fn other_files(&self) -> &[CaptureFileRef] {
        &self.content.other_files
    }
    /// Typed node or directed-edge identity.
    pub fn fact_key(&self) -> &FactKey {
        &self.content.fact_key
    }
    /// Canonical digest of the complete original emitted fact.
    pub fn fact_digest(&self) -> &str {
        &self.content.fact_digest
    }
    /// Complete ordered source inventory; no adjustable caller ranges.
    pub fn ranges(&self) -> &[ReceiptRange] {
        &self.content.ranges
    }
    /// Grammar used for these actual retained bytes.
    pub fn grammar(&self) -> Grammar {
        self.content.grammar
    }
    /// The input closure state of a v4 receipt; `None` for v1 to v3, which
    /// never establish one.
    pub fn closure(&self) -> Option<&ReceiptClosure> {
        match &self.content.input_closure {
            InputClosure::Legacy(_) => None,
            InputClosure::Closure(closure) => Some(closure),
        }
    }

    /// This freshly minted v3 TypeScript receipt under contract v4, with
    /// `closure`: same fact, ranges and files, a new identity (ADR-0072
    /// slice 5). Only a producer that bound the closure's inputs may call
    /// it; a published receipt is never retrofitted.
    pub fn with_closure(self, closure: ReceiptClosure) -> Result<Self, CapturedError> {
        self.validate()?;
        if self.content.schema_version != 3 || !self.content.grammar.has_v4() {
            return Err(CapturedError::Invalid("receipt contract"));
        }
        let current = contract(4, self.content.grammar)?;
        let mut content = self.content;
        content.schema_version = 4;
        content.producer_contract = current.producer.into();
        content.grammar_package = current.grammar_package.into();
        content.parser_package = current.parser_package.into();
        content.input_closure = InputClosure::Closure(closure);
        let receipt = Self {
            receipt_id: content_id(&content)?,
            content,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    /// Bounded wire reader with fixed diagnostics, including validating serde.
    pub fn from_json(json: &str) -> Result<Self, CapturedError> {
        if json.len() > MAX_RECEIPT_BYTES {
            return Err(CapturedError::Limit("receipt bytes"));
        }
        serde_json::from_str(json).map_err(|_| CapturedError::Invalid("receipt metadata"))
    }

    /// Serialize the complete immutable record, never original source text.
    pub fn to_json(&self) -> Result<String, CapturedError> {
        self.validate()?;
        serde_json::to_string(self).map_err(|_| CapturedError::Invalid("receipt serialization"))
    }

    /// Validate content identity/shape only, not producer trust or semantic truth.
    pub fn validate(&self) -> Result<(), CapturedError> {
        let content = &self.content;
        validate_identity(&content.file, &content.repo_key)?;
        let contract = contract(content.schema_version, content.grammar)?;
        match (content.schema_version, &content.commit_sha) {
            (1 | 2, None) => {}
            (3 | 4, Some(commit_sha)) if bounded_identity(commit_sha, MAX_ID_BYTES) => {}
            _ => return Err(CapturedError::Invalid("receipt contract")),
        }
        // v1 to v3 keep their exact bytes; only v4 records a closure state.
        let closure_ok = match (&content.input_closure, content.schema_version) {
            (InputClosure::Legacy(_), 1..=3) => true,
            (InputClosure::Closure(closure), 4) => closure.valid(),
            _ => false,
        };
        if !closure_ok {
            return Err(CapturedError::Invalid("receipt input closure"));
        }
        if content.source_id != content.file.source_id
            || content.producer_contract != contract.producer
            || content.grammar_package != contract.grammar_package
            || (content.parser_package != contract.parser_package
                && !contract
                    .parser_package_legacy
                    .contains(&content.parser_package.as_str()))
            || content.grammar != Grammar::for_file(&content.file)
        {
            return Err(CapturedError::Invalid("receipt contract"));
        }
        let (identities, digest_prefix) = match &content.fact_key {
            FactKey::Node { id } => (vec![id.as_str()], "node-v1:"),
            FactKey::Edge {
                source,
                label,
                destination,
            } => (
                vec![source.as_str(), label.as_str(), destination.as_str()],
                "edge-v1:",
            ),
        };
        if identities
            .iter()
            .any(|value| !bounded_identity(value, MAX_ID_BYTES))
            || !hash_has_prefix(&content.fact_digest, digest_prefix)
            || !hash_has_prefix(&self.receipt_id, contract.prefix)
        {
            return Err(CapturedError::Invalid("receipt fact identity"));
        }
        if content.ranges.is_empty() || content.ranges.len() > MAX_RECEIPT_RANGES {
            return Err(CapturedError::Limit("receipt ranges"));
        }
        if !valid_inventory_shape(&content.ranges, content.schema_version)
            || (content.grammar.provenance_only()
                && content
                    .ranges
                    .iter()
                    .any(|range| range.role != RangeRole::Provenance))
        {
            return Err(CapturedError::Invalid("receipt range inventory order"));
        }
        if let Some(sizes) = &content.source_sizes {
            sizes.validate(content)?;
        }
        validate_other_files(content)?;
        let mut seen = std::collections::BTreeSet::new();
        for range in &content.ranges {
            let evidence = &range.evidence;
            let file = &range.captured.file;
            if (*file != content.file && !content.other_files.contains(file))
                || evidence.repo != content.repo_key
                || evidence.path != file.path
                || !bounded_identity(&evidence.commit_sha, MAX_ID_BYTES)
                || evidence.commit_sha != content.ranges[0].evidence.commit_sha
                || content
                    .commit_sha
                    .as_ref()
                    .is_some_and(|commit_sha| &evidence.commit_sha != commit_sha)
                || evidence.byte_start != range.captured.byte_start
                || evidence.byte_end != range.captured.byte_end
                || evidence.byte_start >= evidence.byte_end
                || evidence.byte_end > file.byte_len
                || evidence.byte_end - evidence.byte_start > source_capture::MAX_SPAN_BYTES
                || !seen.insert((range.role, range.index))
            {
                return Err(CapturedError::Invalid("receipt source range"));
            }
        }
        if serde_json::to_vec(self)
            .map_err(|_| CapturedError::Invalid("receipt serialization"))?
            .len()
            > MAX_RECEIPT_BYTES
        {
            return Err(CapturedError::Limit("receipt bytes"));
        }
        if compact_version(content.schema_version)
            && serde_json::to_vec(&ExpandedReceipt {
                receipt_id: &self.receipt_id,
                content: content.as_full_wire(None),
            })
            .map_err(|_| CapturedError::Invalid("receipt serialization"))?
            .len()
                > MAX_RECEIPT_BYTES
        {
            return Err(CapturedError::Limit("receipt bytes"));
        }
        if self.receipt_id != content_id(content)? {
            return Err(CapturedError::Invalid("receipt content identity"));
        }
        Ok(())
    }

    fn matches_inventory(&self, props: &serde_json::Value) -> bool {
        inventory(props, self.content.schema_version, self.content.grammar).is_ok_and(|ranges| {
            ranges.len() == self.content.ranges.len()
                && ranges.iter().zip(&self.content.ranges).all(
                    |((role, index, evidence), range)| {
                        *role == range.role && *index == range.index && *evidence == range.evidence
                    },
                )
        })
    }

    /// Full content/range match after directory or host enrichment; no trust grant.
    pub fn matches_node(&self, node: &Node) -> bool {
        self.content.fact_key
            == (FactKey::Node {
                id: node.id.clone(),
            })
            && node_digest(node).is_ok_and(|digest| digest == self.content.fact_digest)
            && self.matches_inventory(&node.props)
    }

    /// Full content/range match for the original directed relationship.
    pub fn matches_edge(&self, edge: &Edge) -> bool {
        self.content.fact_key
            == (FactKey::Edge {
                source: edge.src.clone(),
                label: edge.label.clone(),
                destination: edge.dst.clone(),
            })
            && edge_digest(edge).is_ok_and(|digest| digest == self.content.fact_digest)
            && self.matches_inventory(&edge.props)
    }
}

/// An environment reader file that no TS source, config or other grammar
/// claims (AC-1530): such a file keeps its own lane.
fn environment_path(path: &str) -> bool {
    ingest::environments::is_environment_file(path)
        && !super::supported(path)
        && !super::is_config_path(path)
}

/// `receipt` with its last range's role set to `role` (index 0), re-signed,
/// and not validated: a self-consistent identity for tests of content checks.
#[cfg(test)]
pub(super) fn resigned_with_role(receipt: Receipt, role: RangeRole) -> Receipt {
    let mut content = receipt.content;
    let last = content.ranges.last_mut().unwrap();
    last.role = role;
    last.index = 0;
    Receipt {
        receipt_id: content_id(&content).unwrap(),
        content,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rehashed_invalid_receipt_contracts_and_ranges_are_rejected() {
        // AC-0149: validation rejects invalid content even when its hash has
        // been recomputed; a self-consistent digest is not producer authority.
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("source.ts"),
            b"function run(ok: boolean) { if (ok) return false; }",
        )
        .unwrap();
        let source = source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap();
        let capture = source_capture::capture_working_tree(
            root.path(),
            &source,
            &["source.ts".into()],
            source_capture::CaptureLimits::default(),
        )
        .unwrap();
        let (_, receipts) = super::super::extract_file(
            capture.file("source.ts").unwrap(),
            &SourceId {
                repo: "acme/project",
                commit: "workdir",
            },
        )
        .unwrap();
        let original = receipts
            .iter()
            .find(|receipt| receipt.ranges().len() > 1)
            .unwrap();
        let cases: [fn(&mut Content); 7] = [
            |content| content.producer_contract = "t0.adapter-ts/direct-lexical-v1".into(),
            |content| content.grammar = Grammar::Tsx,
            |content| content.ranges[0].index = 1,
            |content| content.ranges.swap(0, 1),
            |content| {
                content.ranges[0].evidence.byte_end = content.file.byte_len + 1;
                content.ranges[0].captured.byte_end = content.file.byte_len + 1;
            },
            |content| content.ranges = vec![content.ranges[0].clone(); MAX_RECEIPT_RANGES + 1],
            |content| {
                content.fact_key = FactKey::Node {
                    id: "x".repeat(MAX_ID_BYTES + 1),
                }
            },
        ];
        for change in cases {
            let mut receipt = original.clone();
            change(&mut receipt.content);
            receipt.receipt_id = content_id(&receipt.content).unwrap();
            assert!(receipt.validate().is_err());
            assert!(Receipt::from_json(&serde_json::to_string(&receipt).unwrap()).is_err());
        }
        for change in [
            (|content: &mut Content| content.ranges[0].captured.file.digest = "0".repeat(64))
                as fn(&mut Content),
            |content| content.ranges[1].evidence.commit_sha = "different".into(),
        ] {
            let mut v3 = original.clone();
            change(&mut v3.content);
            v3.receipt_id = content_id(&v3.content).unwrap();
            assert!(v3.validate().is_err());
            let mut v2 = v3;
            v2.content.schema_version = 2;
            v2.content.producer_contract = "t0.adapter-ts/direct-lexical-v2".into();
            v2.content.commit_sha = None;
            v2.receipt_id = content_id(&v2.content).unwrap();
            assert!(v2.validate().is_err());
            assert!(Receipt::from_json(&serde_json::to_string(&v2).unwrap()).is_err());
        }
        // Omission also fails against the actual complete fact, even if an
        // internally valid provenance-only inventory was rehashed by a caller.
        let (facts, _) = super::super::extract_file(
            capture.file("source.ts").unwrap(),
            &SourceId {
                repo: "acme/project",
                commit: "workdir",
            },
        )
        .unwrap();
        let mut omitted = original.clone();
        omitted
            .content
            .ranges
            .retain(|range| range.role == RangeRole::Provenance);
        omitted.receipt_id = content_id(&omitted.content).unwrap();
        omitted.validate().unwrap();
        let node = facts
            .nodes
            .iter()
            .find(|node| FactKey::from_node(node) == *omitted.fact_key())
            .unwrap();
        assert!(!omitted.matches_node(node));
    }
}

mod minted;
pub use minted::{Citable, MintedHeader, MintedReceipt};
mod workspace;
pub use workspace::{fact_match_workspace, fact_match_workspace_bytes};
#[cfg(test)]
mod v2_tests;
#[cfg(test)]
mod v3_tests;
#[cfg(test)]
mod v4_tests;
