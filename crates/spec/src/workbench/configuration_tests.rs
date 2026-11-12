//! `configuration.md` (#1821, AC-0651).
use super::super::selected;
use super::*;
use crate::{
    FactIndex, FactIndexError, SelectedArtifactOutcome, SelectedCompileLimits, SpecArtifact,
    check_citations, citation_id, compile_spec, compile_spec_selected,
    try_compile_spec_with_coverage,
};
use core_graph::FactKey;
use core_prov::{EvidenceRef, Provenance, Tier};
use serde_json::{Value, json};

const ID: &str = "configuration";
const SHOP: &str = "acme/shop";
const OTHER: &str = "acme/other";

fn prov(
    tier: Tier,
    confidence: ConfidenceTier,
    repo: &str,
    path: &str,
    spans: &[(u64, u64)],
    fact: &str,
) -> Value {
    let evidence = spans
        .iter()
        .map(|&(byte_start, byte_end)| EvidenceRef {
            repo: repo.into(),
            path: path.into(),
            byte_start,
            byte_end,
            commit_sha: "0123abc".into(),
        })
        .collect();
    serde_json::to_value(
        Provenance::new(tier, confidence, evidence, "t0.fixture", fact.as_bytes()).unwrap(),
    )
    .unwrap()
}

fn confirmed(repo: &str, path: &str, spans: &[(u64, u64)], fact: &str) -> Value {
    prov(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        repo,
        path,
        spans,
        fact,
    )
}

fn weak_prov(repo: &str, path: &str, spans: &[(u64, u64)], fact: &str) -> Value {
    prov(
        Tier::Agentic,
        ConfidenceTier::InferredWeak,
        repo,
        path,
        spans,
        fact,
    )
}

fn env_id(repo: &str, path: &str) -> String {
    format!("tool:{repo}@{path}")
}

fn read_id(repo: &str, path: &str, key: Option<&str>) -> String {
    format!("cfgread:{repo}@{path}:env:{}", key.unwrap_or("?"))
}

fn symbol(repo: &str, path: &str, name: &str) -> String {
    format!("sym:{repo}@{path}#{name}")
}

/// Facts in the shape `ingest::toolchain` and `adapters_fw::config_read`
/// emit them.
#[derive(Clone, Default)]
struct Fixture {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Fixture {
    /// An environment file: key names only and no evidence span (AC-0097).
    fn env_file(&mut self, repo: &str, path: &str, keys: &[&str]) {
        let id = env_id(repo, path);
        self.nodes.push(Node {
            props: json!({
                "name": path,
                "display": "Environment file",
                "category": "environment",
                "settings": {"keys": keys},
                "prov": confirmed(repo, path, &[], &id),
            }),
            id,
            label: "Tool".into(),
        });
    }

    /// The reads of `key` in one file, each `(reading symbol, span)`; a
    /// symbol of `None` is a read at file level.
    fn read(
        &mut self,
        (repo, path, language): (&str, &str, &str),
        key: Option<&str>,
        sites: &[(Option<&str>, (u64, u64))],
    ) {
        let id = read_id(repo, path, key);
        // Every read of the key in the file, in byte order.
        let mut spans: Vec<(u64, u64)> = sites.iter().map(|(_, span)| *span).collect();
        spans.sort_unstable();
        let mut props = json!({
            "source": "env",
            "language": language,
            "prov": confirmed(repo, path, &spans, &id),
        });
        match key {
            Some(key) => props["key"] = key.into(),
            None => props["dynamic"] = true.into(),
        }
        self.nodes.push(Node {
            id: id.clone(),
            label: "ConfigRead".into(),
            props,
        });
        for (name, span) in sites {
            let src = match name {
                Some(name) => symbol(repo, path, name),
                None => format!("file:{repo}@{path}"),
            };
            self.edges.push(Edge {
                props: json!({
                    "prov": confirmed(repo, path, &[*span], &format!("{src} READS_CONFIG {id}")),
                }),
                src,
                dst: id.clone(),
                label: "READS_CONFIG".into(),
            });
        }
    }

    fn fact(&mut self, id: &str) -> &mut Node {
        self.nodes.iter_mut().find(|node| node.id == id).unwrap()
    }

    fn artifact(&self, mode: ExportMode) -> SpecArtifact {
        find(compile_spec(
            &self.nodes,
            &self.edges,
            &[],
            mode,
            &BTreeSet::new(),
        ))
    }
}

fn find(bundle: crate::SpecBundle) -> SpecArtifact {
    bundle
        .artifacts
        .into_iter()
        .find(|artifact| artifact.id == ID)
        .expect("the configuration artifact is exported")
}

/// Two repositories. In `acme/shop`: a key declared in two files and read in
/// two, a key declared and never read, a key read and never declared, a
/// name that differs only in case, and one file with unnamed reads. In
/// `acme/other`: a file declaring the key `acme/shop` reads and never
/// declares. Facts are added out of order.
fn shop() -> Fixture {
    let mut f = Fixture::default();
    f.read(
        (SHOP, "src/mail.ts", "typescript"),
        Some("SMTP_HOST"),
        &[(Some("send"), (90, 111)), (Some("probe"), (40, 61))],
    );
    f.env_file(SHOP, "deploy/prod.env", &["DATABASE_URL"]);
    f.read(
        (SHOP, "worker/main.py", "python"),
        Some("DATABASE_URL"),
        &[(None, (12, 40))],
    );
    f.read(
        (SHOP, "src/db.ts", "typescript"),
        None,
        &[(Some("lookup"), (300, 317))],
    );
    f.env_file(OTHER, ".env", &["SMTP_HOST"]);
    f.read(
        (SHOP, "src/db.ts", "typescript"),
        Some("DATABASE_URL"),
        &[(Some("connect"), (120, 144)), (Some("connect"), (200, 224))],
    );
    // Assigned twice in one file: one declaration.
    f.env_file(
        SHOP,
        ".env.example",
        &["DATABASE_URL", "UNUSED_FLAG", "DATABASE_URL"],
    );
    f.read(
        (SHOP, "cmd/main.go", "go"),
        Some("database_url"),
        &[(Some("main"), (55, 80))],
    );
    // One edge per reading symbol, citing each of its reads.
    f.edges
        .dedup_by(|left, right| left.src == right.src && left.dst == right.dst);
    f
}

/// The rows of the table under `## heading`.
fn table<'a>(content: &'a str, heading: &str) -> Vec<&'a str> {
    let start = content
        .find(&format!("\n## {heading}\n"))
        .unwrap_or_else(|| panic!("no section {heading} in:\n{content}"));
    content[start..]
        .lines()
        .skip(1)
        .skip_while(|line| !line.starts_with("|---"))
        .skip(1)
        .take_while(|line| line.starts_with('|'))
        .collect()
}

// AC-0651: each key is listed once per repository with the environment
// files that declare it and the files and symbols that read it, each with
// tier, confidence and span; a key read and never declared and a key
// declared and never read are marked, and the document says what "never
// declared" does and does not mean.
#[test]
fn each_key_is_listed_once_with_where_it_is_declared_and_read() {
    let content = shop().artifact(ExportMode::VerifiedOnly).content;
    assert!(content.contains(
        "Export mode: `verified-only`. Keys: 5. Declared and read: 1. Read, never declared: 2. \
         Declared, never read: 2. Environment files: 3. Files with reads that cannot be named: 1."
    ));
    // By repository, then name; names are compared exactly, and a file in
    // another repository declares nothing here.
    assert_eq!(
        table(&content, "Keys"),
        [
            "| acme/other | SMTP\\_HOST | declared, never read | 1 | 0 | — |",
            "| acme/shop | DATABASE\\_URL | declared and read | 2 | 2 | — |",
            "| acme/shop | SMTP\\_HOST | read, never declared | 0 | 1 | — |",
            "| acme/shop | UNUSED\\_FLAG | declared, never read | 1 | 0 | — |",
            "| acme/shop | database\\_url | read, never declared | 0 | 1 | — |",
        ]
    );
    // An environment file has no span, and the row says so.
    const NO_SPAN: &str = "| Deterministic | Confirmed | none stored for an environment file | — |";
    assert_eq!(
        table(&content, "Where each key is declared"),
        [
            format!("| acme/other | SMTP\\_HOST | .env {NO_SPAN}"),
            format!("| acme/shop | DATABASE\\_URL | .env.example {NO_SPAN}"),
            format!("| acme/shop | DATABASE\\_URL | deploy/prod.env {NO_SPAN}"),
            format!("| acme/shop | UNUSED\\_FLAG | .env.example {NO_SPAN}"),
        ]
    );
    assert_eq!(
        table(&content, "Where each key is read"),
        [
            "| acme/shop | DATABASE\\_URL | src/db.ts | typescript | connect | Deterministic | \
             Confirmed | acme/shop:src/db.ts bytes 120..144 @ 0123abc (+1 more in the fact \
             index) | — |",
            "| acme/shop | DATABASE\\_URL | worker/main.py | python | file level | Deterministic \
             | Confirmed | acme/shop:worker/main.py bytes 12..40 @ 0123abc | — |",
            "| acme/shop | SMTP\\_HOST | src/mail.ts | typescript | probe; send | Deterministic | \
             Confirmed | acme/shop:src/mail.ts bytes 40..61 @ 0123abc (+1 more in the fact \
             index) | — |",
            "| acme/shop | database\\_url | cmd/main.go | go | main | Deterministic | Confirmed | \
             acme/shop:cmd/main.go bytes 55..80 @ 0123abc | — |",
        ]
    );
    assert!(content.contains(
        "- **Read, never declared** means that no environment file in the ingested sources \
         declares the key. It does not mean that the key is unset at run time:"
    ));
    assert!(
        content.contains("- **Declared, never read** means that no recovered read names the key.")
    );
    // Names only: nothing but the key names of an environment file is read.
    let mut f = shop();
    f.fact(&env_id(SHOP, "deploy/prod.env")).props["settings"]["values"] =
        json!(["postgres://user:hunter2@db/shop"]);
    assert!(
        !f.artifact(ExportMode::BestEffort)
            .content
            .contains("hunter2")
    );
}

// AC-0651: a file with reads whose key is not a recordable literal is
// listed, so the document says that keys exist which it cannot name.
#[test]
fn files_with_unnamed_reads_are_listed() {
    let content = shop().artifact(ExportMode::VerifiedOnly).content;
    assert_eq!(
        table(&content, "Files with reads that cannot be named"),
        [
            "| acme/shop | src/db.ts | typescript | lookup | Deterministic | Confirmed | \
          acme/shop:src/db.ts bytes 300..317 @ 0123abc | — |"
        ]
    );
    assert!(content.contains(
        "Keys exist in these files that this document cannot name, and any key marked declared \
         and never read may be one of them."
    ));
    // The unnamed read is not a key.
    assert!(
        !table(&content, "Keys")
            .iter()
            .any(|row| row.contains("| ? |"))
    );

    let mut none = shop();
    none.nodes.retain(|node| node.props["dynamic"] != true);
    none.edges.retain(|edge| !edge.dst.ends_with(":env:?"));
    assert_eq!(
        table(
            &none.artifact(ExportMode::VerifiedOnly).content,
            "Files with reads that cannot be named"
        ),
        ["| — | None recovered | — | — | — | — | — | — |"]
    );
}

// AC-0651: the languages read and the reads that are not covered are stated
// in every export, and an export with no configuration fact says so instead
// of rendering nothing.
#[test]
fn languages_read_and_what_is_not_covered_are_stated() {
    for fixture in [shop(), Fixture::default()] {
        let content = fixture.artifact(ExportMode::VerifiedOnly).content;
        let languages: Vec<&str> = table(&content, "Languages read")
            .iter()
            .map(|row| row.split(" | ").next().unwrap().trim_start_matches("| "))
            .collect();
        assert_eq!(
            languages,
            [
                "TypeScript",
                "JavaScript",
                "Python",
                "Go",
                "Rust",
                "Java",
                "Kotlin"
            ]
        );
        let note = &content[content
            .find("\n## Coverage note: what this document does not cover\n")
            .unwrap()..];
        for stated in [
            "`import.meta.env` and a destructured `process.env`",
            "`\"X\" in os.environ`, in Python is not read",
            "`env!` and `option_env!` macros in Rust are not read",
            "Spring `@Value` and `Environment`, NestJS `ConfigService`, Viper",
            "Only environment files count as declarations.",
            "No value is read.",
        ] {
            assert!(note.contains(stated), "{stated}");
        }
    }
    let empty = Fixture::default().artifact(ExportMode::BestEffort);
    assert_eq!(
        table(&empty.content, "Keys"),
        ["| — | None in this export | — | 0 | 0 | — |"]
    );
    assert!(empty.assertions.is_empty());

    // A read from a source this version does not project, a fact whose id
    // names no file and a relation to a read that is not listed are counted,
    // never listed as keys.
    let mut f = Fixture::default();
    f.read(
        (SHOP, "src/app.ts", "typescript"),
        Some("PORT"),
        &[(Some("boot"), (1, 9))],
    );
    f.nodes[0].props["source"] = "spring".into();
    f.nodes.push(Node {
        id: "cfgread:nowhere".into(),
        label: "ConfigRead".into(),
        props: json!({"source": "env", "key": "LOST"}),
    });
    let content = f.artifact(ExportMode::BestEffort).content;
    assert!(content.contains("Keys: 0."));
    assert!(content.contains("1 admitted configuration facts are not listed:"));
    assert!(content.contains("1 admitted configuration-read facts are not listed:"));
    assert!(content.contains("1 admitted reading relations are not listed:"));
}

// AC-0651 (R-INT-5): a verified-only export leaves out InferredWeak rows and
// states how many; a best-effort export includes them, marked. A rejected
// inference is withheld and counted in both.
#[test]
fn verified_only_leaves_out_weak_rows_and_counts_them_and_best_effort_marks_them() {
    let mut f = shop();
    // The only read of SMTP_HOST, and one of the two declarations of
    // DATABASE_URL, are weak.
    let weak_read = read_id(SHOP, "src/mail.ts", Some("SMTP_HOST"));
    f.fact(&weak_read).props["prov"] = weak_prov(SHOP, "src/mail.ts", &[(40, 61)], &weak_read);
    let weak_file = env_id(SHOP, "deploy/prod.env");
    f.fact(&weak_file).props["prov"] = weak_prov(SHOP, "deploy/prod.env", &[], &weak_file);

    let verified = find(
        try_compile_spec_with_coverage(
            &f.nodes,
            &f.edges,
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
            &[],
        )
        .expect("withheld rows cite nothing, so the export is not refused"),
    );
    let content = &verified.content;
    assert!(content.contains(
        "Configuration facts left out of this export because they are InferredWeak or \
         rejected: 2."
    ));
    // The key's read is gone, so the key is; its relations are counted.
    assert!(!content.contains("SMTP\\_HOST | read"));
    assert!(content.contains("2 admitted reading relations are not listed:"));
    assert!(content.contains("| acme/shop | DATABASE\\_URL | declared and read | 1 | 2 | — |"));
    assert!(!content.contains("deploy/prod.env"));
    assert!(!content.contains("InferredWeak fact;") && !content.contains("InferredWeak facts"));
    assert!(verified.assertions.iter().all(|assertion| {
        assertion.provenance.confidence_tier != ConfidenceTier::InferredWeak
            && !assertion.subject_id.contains("src/mail.ts")
            && assertion.subject_id != weak_file
    }));

    let best = f.artifact(ExportMode::BestEffort);
    let content = &best.content;
    assert!(content.contains(
        "InferredWeak rows are included and marked in the Note column. Configuration facts left \
         out of this export because they are rejected: 0."
    ));
    assert!(!content.contains("are not listed"));
    const MARK: &str = "| rests on an InferredWeak fact; a verified-only export omits this row |";
    let declared = table(content, "Where each key is declared");
    let row = declared
        .iter()
        .find(|row| row.contains("deploy/prod.env"))
        .unwrap();
    assert!(row.contains("| Agentic | InferredWeak |") && row.ends_with(MARK));
    let reads = table(content, "Where each key is read");
    let row = reads
        .iter()
        .find(|row| row.contains("src/mail.ts"))
        .unwrap();
    assert!(row.contains("| Agentic | InferredWeak |") && row.ends_with(MARK));
    assert_eq!(content.matches(MARK).count(), 2);
    let keys = table(content, "Keys");
    assert!(keys.contains(
        &"| acme/shop | DATABASE\\_URL | declared and read | 2 | 2 | InferredWeak facts among \
          these: 1; a verified-only export omits them, which can change this status |"
    ));
    assert!(keys.iter().any(|row| row.starts_with(
        "| acme/shop | SMTP\\_HOST | read, never declared | 0 | 1 | InferredWeak facts among \
         these: 1;"
    )));

    // A weak reading relation is marked where it is listed; verified-only
    // drops it and keeps the read.
    let mut f = shop();
    let relation = f
        .edges
        .iter_mut()
        .find(|edge| edge.src == symbol(SHOP, "src/mail.ts", "probe"))
        .unwrap();
    relation.props["prov"] = weak_prov(SHOP, "src/mail.ts", &[(40, 61)], "weak relation");
    let best = f.artifact(ExportMode::BestEffort).content;
    assert!(
        best.contains("| probe [InferredWeak; a verified-only export leaves this out]; send |")
    );
    let verified = f.artifact(ExportMode::VerifiedOnly).content;
    assert!(verified.contains("InferredWeak or rejected: 1."));
    assert!(verified.contains("| acme/shop | SMTP\\_HOST | src/mail.ts | typescript | send |"));

    // A rejected InferredStrong fact is withheld and counted in both modes.
    let mut f = shop();
    let strong = prov(
        Tier::Semantic,
        ConfidenceTier::InferredStrong,
        SHOP,
        "cmd/main.go",
        &[(55, 80)],
        "strong read",
    );
    let hash = strong["content_hash"].as_str().unwrap().to_owned();
    f.fact(&read_id(SHOP, "cmd/main.go", Some("database_url")))
        .props["prov"] = strong;
    let rejected = find(compile_spec(
        &f.nodes,
        &f.edges,
        &[],
        ExportMode::BestEffort,
        &BTreeSet::from([hash]),
    ));
    assert!(rejected.content.contains("because they are rejected: 1."));
    assert!(!rejected.content.contains("database\\_url"));
}

// AC-0651: the same graph yields the same bytes whatever order the store or
// the caller hands the facts over in, and the selected compiler agrees.
#[test]
fn two_exports_are_byte_identical_whatever_the_input_order() {
    let f = shop();
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let first = f.artifact(mode);
        assert_eq!(f.artifact(mode), first);
        let mut reversed = Fixture {
            nodes: f.nodes.iter().rev().cloned().collect(),
            edges: f.edges.iter().rev().cloned().collect(),
        };
        // A fact handed over twice is listed once.
        reversed.nodes.extend(f.nodes.iter().take(3).cloned());
        assert_eq!(reversed.artifact(mode), first);
        assert!(first.verify_identity());

        let selected = compile_spec_selected(
            &reversed.nodes,
            &reversed.edges,
            &[],
            mode,
            &BTreeSet::new(),
            &[ID],
            SelectedCompileLimits {
                max_bytes: 1 << 20,
                adr: crate::AdrLimits {
                    max_rows: 1000,
                    max_output_bytes: 1 << 20,
                },
            },
        )
        .unwrap();
        assert_eq!(selected, [SelectedArtifactOutcome::Produced(first)]);
    }
}

// AC-0651 (ADR-0058): every assertion of the artifact has a row in the
// export's fact index that names this artifact, the ids it cites pass the
// export's citation check, and two facts sharing one id fail the export
// instead of producing a document.
#[test]
fn every_row_cites_a_fact_that_resolves_in_the_fact_index() {
    let f = shop();
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let bundle =
            try_compile_spec_with_coverage(&f.nodes, &f.edges, &[], mode, &BTreeSet::new(), &[])
                .expect("the citations resolve");
        let artifact = bundle.artifacts.iter().find(|a| a.id == ID).unwrap();
        let index = bundle
            .artifacts
            .iter()
            .find(|a| a.id == "fact-index-json")
            .unwrap();
        let index: FactIndex = serde_json::from_str(&index.content).unwrap();
        // 3 environment files, 5 reads and 6 reading relations, each once.
        assert_eq!(artifact.assertions.len(), 3 + 5 + 6);
        let mut cited = Vec::new();
        for assertion in &artifact.assertions {
            let subject = |key: &FactKey| match key {
                FactKey::Node { id } => id.clone(),
                FactKey::Edge {
                    source,
                    label,
                    destination,
                } => format!("{source} {label} {destination}"),
            };
            let row = index
                .rows
                .iter()
                .find(|row| subject(&row.fact) == assertion.subject_id)
                .unwrap_or_else(|| panic!("{} has a row", assertion.subject_id));
            assert_eq!(row.citation_id, citation_id(&row.fact));
            assert!(row.cited_by.iter().any(|by| by == ID));
            assert!(row.records.iter().any(|record| {
                record.tier == assertion.provenance.tier
                    && record.confidence == assertion.provenance.confidence_tier
                    && record.evidence == assertion.provenance.evidence
            }));
            cited.push(row.citation_id.clone());
        }
        check_citations(&index, cited.iter().map(String::as_str)).expect("every id resolves");
        // An environment file is cited here and by the toolchain artifact.
        let file = index
            .rows
            .iter()
            .find(|row| {
                row.fact
                    == FactKey::Node {
                        id: env_id(SHOP, ".env.example"),
                    }
            })
            .unwrap();
        assert!(file.cited_by.iter().any(|by| by == "toolchain"));
        let unknown = FactKey::Node {
            id: read_id(SHOP, "src/db.ts", Some("MISSING")),
        };
        assert_eq!(
            check_citations(&index, [citation_id(&unknown).as_str()]),
            Err(FactIndexError::Unresolved {
                citation_id: citation_id(&unknown)
            })
        );
    }

    // The compile path refuses an export whose citations collide.
    fn collide(_: &FactKey) -> String {
        "F-0000000000000000".into()
    }
    let none = BTreeSet::new();
    let mode = ExportMode::BestEffort;
    let projection = selected::project(&f.nodes, &f.edges, &[], mode, &none, None).unwrap();
    let nodes = super::super::filter_nodes(&projection.nodes, mode, &none);
    let edges = super::super::filter_edges(&projection.edges, mode, &none);
    let error = selected::bundle_with_citation_ids(
        &nodes,
        &edges,
        &projection.flows,
        (mode, &none, Default::default()),
        collide,
    )
    .expect_err("no bundle, and so no configuration.md, for a collision");
    assert!(matches!(error, FactIndexError::Collision { .. }));
}
