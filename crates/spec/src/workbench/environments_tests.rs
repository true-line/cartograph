//! `environments.md` (#1791, AC-0661).
use super::*;
use crate::{
    FactIndex, FactIndexError, SelectedArtifactOutcome, SelectedCompileLimits, SpecArtifact,
    check_citations, citation_id, compile_spec, compile_spec_selected,
    try_compile_spec_with_coverage,
};
use core_graph::FactKey;
use core_prov::{EvidenceRef, Provenance, Tier};
use serde_json::{Value, json};

const ID: &str = "environments";
const SHOP: &str = "acme/shop";
const OTHER: &str = "acme/other";
const BASE: &str = "docker-compose.yml";
const PROD: &str = "deploy/docker-compose.prod.yml";

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
        Provenance::new(
            tier,
            confidence,
            evidence,
            "t0.environments",
            fact.as_bytes(),
        )
        .unwrap(),
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

fn environment_id(repo: &str, path: &str) -> String {
    format!("env:{repo}@{path}")
}

fn workload_id(repo: &str, path: &str, name: &str) -> String {
    format!("workload:{repo}@{path}#{name}")
}

/// Facts in the shape `ingest::environments` emits them.
#[derive(Clone, Default)]
struct Fixture {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Fixture {
    fn environment(&mut self, repo: &str, path: &str, name: Option<&str>, span: (u64, u64)) {
        let id = environment_id(repo, path);
        let mut props = json!({
            "declared_by": "compose",
            "path": path,
            "prov": confirmed(repo, path, &[span], &id),
        });
        if let Some(name) = name {
            props["name"] = name.into();
            props["name_span"] = json!([0, 10]);
        }
        self.nodes.push(Node {
            id,
            label: "Environment".into(),
            props,
        });
    }

    /// A workload and its `DECLARES` relation. `props` are the properties
    /// the file writes.
    fn workload(&mut self, repo: &str, path: &str, name: &str, span: (u64, u64), props: Value) {
        let id = workload_id(repo, path, name);
        let mut props = props;
        props["kind"] = "compose-service".into();
        props["name"] = name.into();
        props["path"] = path.into();
        props["prov"] = confirmed(repo, path, &[span], &id);
        self.nodes.push(Node {
            id: id.clone(),
            label: "Workload".into(),
            props,
        });
        let src = environment_id(repo, path);
        self.edges.push(Edge {
            props: json!({
                "prov": confirmed(repo, path, &[(span.0, span.0 + 3)], &format!("{src} DECLARES {id}")),
            }),
            src,
            dst: id,
            label: "DECLARES".into(),
        });
    }

    fn depends(&mut self, repo: &str, path: &str, from: &str, to: &str, span: (u64, u64)) {
        let src = workload_id(repo, path, from);
        let dst = workload_id(repo, path, to);
        self.edges.push(Edge {
            props: json!({
                "prov": confirmed(repo, path, &[span], &format!("{src} DEPENDS_ON {dst}")),
            }),
            src,
            dst,
            label: "DEPENDS_ON".into(),
        });
    }

    fn gap(
        &mut self,
        repo: &str,
        path: &str,
        (kind, service): (&str, Option<&str>),
        reason: &str,
        spans: &[(u64, u64)],
    ) {
        let mut id = format!("gap:environment:{repo}@{path}");
        let mut props = json!({
            "kind": kind,
            "path": path,
            "reason": reason,
            "attempted_tiers": ["T0"],
        });
        if let Some(service) = service {
            id = format!("{id}#{service}");
            props["service"] = service.into();
        }
        props["prov"] = prov(
            Tier::Deterministic,
            ConfidenceTier::Gap,
            repo,
            path,
            spans,
            &id,
        );
        self.nodes.push(Node {
            id,
            label: "Gap".into(),
            props,
        });
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
        .expect("the environments artifact is exported")
}

/// Two repositories. In `acme/shop`: a Compose file that states no name and
/// one under `deploy/` whose path holds "prod" and that states none either;
/// both declare `web` (with a different image and ports) and `db` (the
/// same); the first also declares `api`, with an interpolated image and a
/// dependency on a service it does not declare. One name-only environment
/// file and one unreadable Compose file. In `acme/other`: a Compose file
/// that states the name `billing` and declares its own `web`. Facts are
/// added out of order.
fn shop() -> Fixture {
    let mut f = Fixture::default();
    f.workload(
        SHOP,
        PROD,
        "web",
        (120, 220),
        json!({"image": "shop/web:2", "ports": ["443:8443"], "environment_keys": ["SESSION_KEY"]}),
    );
    f.environment(SHOP, BASE, None, (0, 8));
    f.workload(
        SHOP,
        BASE,
        "web",
        (20, 140),
        json!({
            "image": "shop/web:1",
            "build": "./web",
            "ports": ["8080:80", "target=443 published=8443"],
            "environment_keys": ["SESSION_KEY"],
            "env_files": ["web.env"],
            "depends_on": ["api", "db"],
            "profiles": ["frontend"],
        }),
    );
    f.gap(
        SHOP,
        "staging.env",
        ("name-only-environment", None),
        "an environment is named only by this file's name; no file declares it",
        &[],
    );
    f.workload(
        SHOP,
        BASE,
        "db",
        (300, 330),
        json!({"image": "postgres:16"}),
    );
    f.workload(
        SHOP,
        PROD,
        "db",
        (230, 260),
        json!({"image": "postgres:16"}),
    );
    f.environment(SHOP, PROD, None, (0, 8));
    f.workload(
        SHOP,
        BASE,
        "api",
        (150, 290),
        json!({
            "image": "shop/api:${TAG}",
            "interpolated": {"image": true},
            "depends_on": ["cache", "db"],
        }),
    );
    f.depends(SHOP, BASE, "web", "db", (130, 132));
    f.depends(SHOP, BASE, "web", "api", (125, 128));
    f.depends(SHOP, BASE, "api", "db", (280, 282));
    f.gap(
        SHOP,
        BASE,
        ("compose-service", Some("api")),
        "`depends_on` names `cache`, a service this file does not declare",
        &[(270, 275)],
    );
    f.gap(
        SHOP,
        "broken/compose.yaml",
        ("compose-file", None),
        "the file is not valid YAML",
        &[(0, 40)],
    );
    f.environment(OTHER, "compose.yaml", Some("billing"), (12, 20));
    f.workload(
        OTHER,
        "compose.yaml",
        "web",
        (30, 60),
        json!({"image": "billing:3"}),
    );
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

// AC-0661: the document states the counts, lists each environment by path
// with its stated name or "none stated", and lists each workload under its
// environment with what the file writes, each row with tier, confidence and
// span.
#[test]
fn environments_and_their_workloads_are_listed_with_spans() {
    let f = shop();
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let content = f.artifact(mode).content;
        assert!(content.starts_with("# Environments\n"));
        assert!(content.contains(
            "Environments: 3. Workloads: 6. Environments named only by a file name: 1. Other \
             items not established: 2."
        ));

        assert_eq!(
            table(&content, "Environments"),
            [
                "| acme/other | compose.yaml | billing | compose | 1 | Deterministic | Confirmed \
                 | acme/other:compose.yaml bytes 12..20 @ 0123abc | — |",
                "| acme/shop | deploy/docker-compose.prod.yml | none stated | compose | 2 | \
                 Deterministic | Confirmed | acme/shop:deploy/docker-compose.prod.yml bytes \
                 0..8 @ 0123abc | — |",
                "| acme/shop | docker-compose.yml | none stated | compose | 3 | Deterministic | \
                 Confirmed | acme/shop:docker-compose.yml bytes 0..8 @ 0123abc | — |",
            ]
        );

        let workloads = table(&content, "Workloads by environment");
        assert_eq!(workloads.len(), 6);
        assert_eq!(
            workloads[5],
            "| acme/shop | docker-compose.yml | web | shop/web:1 | ./web | 8080:80, target=443 \
             published=8443 | SESSION\\_KEY | web.env | api, db | frontend | Deterministic | \
             Confirmed | acme/shop:docker-compose.yml bytes 20..140 @ 0123abc | — |"
        );
        // An interpolated image is shown as written and marked; a dependency
        // with no relation points at the Gap.
        assert_eq!(
            workloads[3],
            "| acme/shop | docker-compose.yml | api | shop/api:${TAG} (holds `${`; not resolved) \
             | — | — | — | — | cache (no relation: see Not established), db | — | Deterministic \
             | Confirmed | acme/shop:docker-compose.yml bytes 150..290 @ 0123abc | — |"
        );
        assert!(workloads[0].starts_with("| acme/other | compose.yaml | web | billing:3 |"));
    }
}

// AC-0661 (ADR-0070 decision 6): a workload name that two environments of
// one repository declare has one row per declaration (each workload fact),
// with its kind, the file that declares it, the environments that reach it,
// the properties that differ and its span; the document never says one
// overrides another, and never joins repositories.
#[test]
fn a_workload_declared_in_two_environments_shows_both_declarations() {
    let f = shop();
    let content = f.artifact(ExportMode::BestEffort).content;
    assert_eq!(
        table(&content, "Declared in more than one environment"),
        [
            "| acme/shop | db | Compose service | deploy/docker-compose.prod.yml | \
             deploy/docker-compose.prod.yml | none: the recorded properties are the same in \
             each declaration | Deterministic | Confirmed | \
             acme/shop:deploy/docker-compose.prod.yml bytes 230..260 @ 0123abc | — |",
            "| acme/shop | db | Compose service | docker-compose.yml | docker-compose.yml | \
             none: the recorded properties are the same in each declaration | Deterministic | \
             Confirmed | acme/shop:docker-compose.yml bytes 300..330 @ 0123abc | — |",
            "| acme/shop | web | Compose service | deploy/docker-compose.prod.yml | \
             deploy/docker-compose.prod.yml | image: shop/web:2; build context: not stated; \
             ports: 443:8443; environment files: not stated; dependencies: not stated; \
             profiles: not stated | Deterministic | Confirmed | \
             acme/shop:deploy/docker-compose.prod.yml bytes 120..220 @ 0123abc | — |",
            "| acme/shop | web | Compose service | docker-compose.yml | docker-compose.yml | \
             image: shop/web:1; build context: ./web; ports: 8080:80, target=443 \
             published=8443; environment files: web.env; dependencies: api, db; profiles: \
             frontend | Deterministic | Confirmed | acme/shop:docker-compose.yml bytes 20..140 @ \
             0123abc | — |",
        ]
    );
    // `web` of `acme/other` is another repository's and is not joined.
    assert!(
        !content[content.find("## Declared in more than one").unwrap()
            ..content.find("## Not established").unwrap()]
            .contains("acme/other")
    );
    let lower = content.to_lowercase();
    for word in [
        "overrides",
        "override",
        "overridden",
        "takes precedence",
        "effective image",
    ] {
        assert!(!lower.contains(word), "{word}");
    }
    assert!(content.contains("This section does not say which declaration applies where."));
}

// AC-0661 (ADR-0070 decisions 2 and 3): no path is shown as an environment
// name. A file under a `prod` path that states no name reads "none stated",
// and a name-only environment file is a Gap row, never an environment.
#[test]
fn no_path_is_shown_as_an_environment_name() {
    let f = shop();
    let content = f.artifact(ExportMode::BestEffort).content;
    for row in table(&content, "Environments") {
        let cells: Vec<&str> = row.split(" | ").collect();
        assert!(cells[2] == "none stated" || cells[2] == "billing", "{row}");
    }
    assert!(!content.contains("| prod |"));
    assert!(!content.contains("| staging |"));
    assert!(!content.contains("| production |"));
    assert_eq!(
        table(&content, "Not established"),
        [
            "| acme/shop | broken/compose.yaml | — | Compose file | the file is not valid YAML | \
             Deterministic | Gap | acme/shop:broken/compose.yaml bytes 0..40 @ 0123abc |",
            "| acme/shop | docker-compose.yml | api | Compose service | \\`depends\\_on\\` names \
             \\`cache\\`, a service this file does not declare | Deterministic | Gap | \
             acme/shop:docker-compose.yml bytes 270..275 @ 0123abc |",
            "| acme/shop | staging.env | — | environment named only by a file name | an \
             environment is named only by this file's name; no file declares it | \
             Deterministic | Gap | none stored for an environment file |",
        ]
    );
    assert!(
        table(&content, "Environments")
            .iter()
            .all(|row| !row.contains("staging.env"))
    );
}

// AC-0661: the document says what an environment is here and what is not
// read, and an export with no environment fact still renders every section.
#[test]
fn what_is_not_read_is_stated_and_an_empty_export_renders_every_section() {
    let empty = Fixture::default().artifact(ExportMode::VerifiedOnly);
    assert!(empty.assertions.is_empty());
    let content = empty.content;
    assert!(content.contains("Environments: 0. Workloads: 0."));
    for heading in [
        "How to read this document",
        "Environments",
        "Workloads by environment",
        "Declared in more than one environment",
        "Workloads no environment declares",
        "Not established",
        "Coverage note: what this document does not cover",
    ] {
        assert!(content.contains(&format!("\n## {heading}\n")), "{heading}");
    }
    for heading in [
        "Environments",
        "Workloads by environment",
        "Declared in more than one environment",
        "Workloads no environment declares",
        "Not established",
    ] {
        let rows = table(&content, heading);
        assert_eq!(rows.len(), 1, "{heading}");
        assert!(rows[0].contains("None in this export"));
    }
    for stated in [
        "A file or directory name is never used as an environment name.",
        // AC-0662 and AC-1502: manifests, kustomize files and Helm charts are read now.
        "Helm charts are read from `Chart.yaml` and the key paths of their values files",
        "Nothing is merged.",
        "No value of an environment key is read or stored, and no secret.",
        "Terraform resources are not attached to an environment.",
    ] {
        assert!(content.contains(stated), "{stated}");
    }
}

// AC-0661 (R-INT-5): verified-only leaves an InferredWeak fact out and
// counts it; best-effort includes it, marked.
#[test]
fn verified_only_leaves_out_weak_rows_and_counts_them_and_best_effort_marks_them() {
    let mut f = shop();
    let id = workload_id(OTHER, "compose.yaml", "web");
    f.fact(&id).props["prov"] = prov(
        Tier::Agentic,
        ConfidenceTier::InferredWeak,
        OTHER,
        "compose.yaml",
        &[(30, 60)],
        &id,
    );
    let verified = f.artifact(ExportMode::VerifiedOnly).content;
    assert!(verified.contains("Workloads: 5."));
    assert!(verified.contains(
        "Environment facts left out of this export because they are InferredWeak or rejected: 1."
    ));
    assert!(!verified.contains("billing:3"));
    let best = f.artifact(ExportMode::BestEffort).content;
    assert!(best.contains("Workloads: 6."));
    let row = table(&best, "Workloads by environment")[0];
    assert!(row.contains("| Agentic | InferredWeak |"));
    assert!(
        row.ends_with("| rests on an InferredWeak fact; a verified-only export omits this row |")
    );
}

// AC-0661: the same graph yields the same bytes whatever order the store or
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
        reversed.edges.extend(f.edges.iter().take(3).cloned());
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

// AC-0661, AC-0662: `Workload` facts and their `DEPENDS_ON` relations, and
// the kustomize facts (several `DECLARES` relations into one workload, and
// `PATCHES`), change no other document: every graph document but the gap
// register, which lists the new Gaps like any other, is the same bytes with
// and without them.
#[test]
fn workload_dependencies_change_neither_the_topology_nor_the_flows() {
    let mut f = shop();
    let kustomize = kustomize::overlays();
    f.nodes.extend(kustomize.nodes);
    f.edges.extend(kustomize.edges);
    assert!(f.edges.iter().any(|edge| edge.label == "PATCHES"));
    let resource = |id: &str| Node {
        id: id.into(),
        label: "Resource".into(),
        props: json!({"prov": confirmed(SHOP, "main.tf", &[(0, 5)], id)}),
    };
    let infra = vec![resource("res:acme/shop@a"), resource("res:acme/shop@b")];
    let infra_edges = vec![Edge {
        src: "res:acme/shop@a".into(),
        dst: "res:acme/shop@b".into(),
        label: "DEPENDS_ON".into(),
        props: json!({"prov": confirmed(SHOP, "main.tf", &[(0, 5)], "a DEPENDS_ON b")}),
    }];
    let mut nodes = infra.clone();
    nodes.extend(f.nodes.iter().cloned());
    let mut edges = infra_edges.clone();
    edges.extend(f.edges.iter().cloned());
    assert!(flowtracer::trace(&nodes, &edges).is_empty());
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let without = compile_spec(&infra, &infra_edges, &[], mode, &BTreeSet::new());
        let with = compile_spec(&nodes, &edges, &[], mode, &BTreeSet::new());
        for id in [
            "topology",
            "flow-dossiers",
            "data-model",
            "security-view",
            "user-stories",
            "us-tm",
            "us-tm-index",
            "adrs",
            "drift-register",
            "toolchain",
            "test-arrangements",
            "journeys",
            "rule-evidence",
            "rule-evidence-index",
            "endpoint-permissions",
            "pipelines",
            "configuration",
            "external",
        ] {
            let pick = |bundle: &crate::SpecBundle| {
                bundle
                    .artifacts
                    .iter()
                    .find(|artifact| artifact.id == id)
                    .unwrap()
                    .clone()
            };
            assert_eq!(pick(&with), pick(&without), "{id}");
        }
        assert!(
            !crate::topology_mermaid(&nodes, &edges).contains("workload"),
            "no workload joins the topology map"
        );
    }
}

// AC-0661 (ADR-0058): every assertion of the artifact has a row in the
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
        // 3 environments, 6 workloads, 6 DECLARES, 3 DEPENDS_ON and 3 Gaps.
        assert_eq!(artifact.assertions.len(), 3 + 6 + 6 + 3 + 3);
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
        let unknown = FactKey::Node {
            id: workload_id(SHOP, BASE, "missing"),
        };
        assert_eq!(
            check_citations(&index, [citation_id(&unknown).as_str()]),
            Err(FactIndexError::Unresolved {
                citation_id: citation_id(&unknown)
            })
        );
    }
}

#[path = "environments_kustomize_tests.rs"]
mod kustomize;

// AC-1502 (T-1502, #2167): a helmfile release is an environment listed by
// its helmfile path and release; the Helm chart it installs is listed under
// it with the chart's default values keys and the key paths of each values
// file the release names; a chart no release installs is listed with no
// environment. No value is shown.
#[test]
fn helmfile_releases_list_their_chart_and_values_keys() {
    const HELMFILE: &str = "deploy/helmfile.yaml";
    const CHART: &str = "charts/shop/Chart.yaml";
    let mut f = Fixture::default();
    let release = format!("{HELMFILE}#shop-prod");
    let env = environment_id(SHOP, &release);
    f.nodes.push(Node {
        id: env.clone(),
        label: "Environment".into(),
        props: json!({
            "declared_by": "helmfile",
            "path": release,
            "file": HELMFILE,
            "release": "shop-prod",
            "name": "shop-production",
            "prov": confirmed(SHOP, HELMFILE, &[(10, 90)], &env),
        }),
    });
    for (path, name) in [(CHART, "shop"), ("charts/cache/Chart.yaml", "cache")] {
        let id = format!("workload:{SHOP}@{path}");
        f.nodes.push(Node {
            props: json!({
                "kind": "helm-chart",
                "name": name,
                "path": path,
                "version": "1.2.0",
                "values_keys": ["image.tag", "replicaCount"],
                "prov": confirmed(SHOP, path, &[(0, 40)], &id),
            }),
            id,
            label: "Workload".into(),
        });
    }
    let chart = format!("workload:{SHOP}@{CHART}");
    f.edges.push(Edge {
        props: json!({
            "values": [{"path": "charts/shop/values-prod.yaml", "keys": ["replicaCount"]}],
            "prov": confirmed(SHOP, HELMFILE, &[(10, 90)], &format!("{env} DECLARES {chart}")),
        }),
        src: env.clone(),
        dst: chart,
        label: "DECLARES".into(),
    });
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let content = f.artifact(mode).content;
        assert!(
            content.contains("Environments: 1. Workloads: 2."),
            "{content}"
        );
        assert_eq!(
            table(&content, "Environments"),
            [
                "| acme/shop | deploy/helmfile.yaml\\#shop-prod | shop-production | helmfile | 1 | \
              Deterministic | Confirmed | acme/shop:deploy/helmfile.yaml bytes 10..90 @ 0123abc \
              | — |"
            ]
        );
        assert!(
            content.contains(
                "| acme/shop | deploy/helmfile.yaml\\#shop-prod | shop | charts/shop/Chart.yaml | \
             1.2.0 | — | image.tag, replicaCount | charts/shop/values-prod.yaml: replicaCount | \
             Deterministic | Confirmed | acme/shop:charts/shop/Chart.yaml bytes 0..40 @ 0123abc \
             | — |"
            ),
            "{content}"
        );
        assert!(
            content.contains(
                "| acme/shop | no environment | cache | charts/cache/Chart.yaml | 1.2.0 | — | \
             image.tag, replicaCount | — |"
            ),
            "{content}"
        );
    }
}

// AC-1503 (T-1503, #2167): a Spring Boot profile environment is listed with
// the profile it states, where it is declared and the key paths it sets.
#[test]
fn spring_profiles_list_their_settings_keys() {
    const BASE: &str = "src/main/resources/application.yml";
    const PROD: &str = "src/main/resources/application-prod.yml";
    let mut f = Fixture::default();
    let staging_path = format!("{BASE}#staging");
    for (path, name, declared_in, keys) in [
        (
            staging_path.as_str(),
            "staging",
            None,
            json!(["server.port"]),
        ),
        (
            PROD,
            "prod",
            Some(BASE),
            json!(["datasource.password", "datasource.url"]),
        ),
    ] {
        let env = environment_id(SHOP, path);
        let mut props = json!({
            "declared_by": "spring-profile",
            "path": path,
            "name": name,
            "settings_keys": keys,
            "prov": confirmed(SHOP, BASE, &[(20, 40)], &env),
        });
        match declared_in {
            Some(base) => props["profile_declared_in"] = base.into(),
            None => props["file"] = BASE.into(),
        }
        f.nodes.push(Node {
            id: env,
            label: "Environment".into(),
            props,
        });
    }
    let content = f.artifact(ExportMode::BestEffort).content;
    assert!(
        content.contains(
            "| acme/shop | src/main/resources/application-prod.yml | prod | \
             src/main/resources/application.yml | datasource.password, datasource.url | \
             Deterministic | Confirmed | acme/shop:src/main/resources/application.yml bytes \
             20..40 @ 0123abc | — |"
        ),
        "{content}"
    );
    assert!(
        content.contains(
            "| acme/shop | src/main/resources/application.yml\\#staging | staging | \
             src/main/resources/application.yml | server.port |"
        ),
        "{content}"
    );
    assert!(content.contains("Spring Boot `application` files are read"));
}
