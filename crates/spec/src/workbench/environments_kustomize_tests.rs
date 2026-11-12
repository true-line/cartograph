//! `environments.md` over kustomize and Kubernetes facts (#1791, AC-0662).
use super::*;

const BASE: &str = "k8s/base/kustomization.yaml";
const STAGING: &str = "k8s/overlays/staging/kustomization.yaml";
const PROD: &str = "k8s/overlays/prod/kustomization.yaml";
const DEPLOYMENT: &str = "k8s/base/deployment.yaml";
const PATCH: &str = "k8s/overlays/staging/patch.yaml";

fn manifest_id(path: &str, document: usize, kind: &str, name: &str) -> String {
    format!("workload:{SHOP}@{path}#{document}:{kind}/{name}")
}

impl Fixture {
    fn kustomization(&mut self, path: &str, props: Value) {
        let id = environment_id(SHOP, path);
        let mut props = props;
        props["declared_by"] = "kustomize".into();
        props["path"] = path.into();
        props["prov"] = confirmed(SHOP, path, &[(0, 9)], &id);
        self.nodes.push(Node {
            id,
            label: "Environment".into(),
            props,
        });
    }

    fn manifest(&mut self, path: &str, (document, kind, name): (usize, &str, &str), props: Value) {
        let id = manifest_id(path, document, kind, name);
        let mut props = props;
        props["kind"] = format!("k8s-{}", kind.to_lowercase()).into();
        props["name"] = name.into();
        props["path"] = path.into();
        props["prov"] = confirmed(SHOP, path, &[(0, 400)], &id);
        self.nodes.push(Node {
            id,
            label: "Workload".into(),
            props,
        });
    }

    /// A relation from the kustomization at `from` to a workload, cited in
    /// `cited`.
    fn relation(&mut self, from: &str, label: &str, to: &str, cited: (&str, u64), props: Value) {
        let src = environment_id(SHOP, from);
        let mut props = props;
        props["prov"] = confirmed(
            SHOP,
            cited.0,
            &[(cited.1, cited.1 + 10)],
            &format!("{src} {label} {to}"),
        );
        self.edges.push(Edge {
            src,
            dst: to.into(),
            label: label.into(),
            props,
        });
    }
}

/// A base and two overlays in `acme/shop`, in the shape `ingest` emits. The
/// base declares `Deployment/web`; both overlays reach it through the base.
/// Staging states a namespace, two patch documents and an `images` entry
/// naming the workload's image. Prod states no name, no patch, an `images`
/// entry naming another image, and a `namePrefix`. A `Job` in another file
/// is reached by nothing and states no containers. Facts are added out of
/// order.
pub(super) fn overlays() -> Fixture {
    let mut f = Fixture::default();
    let web = manifest_id(DEPLOYMENT, 0, "Deployment", "web");
    f.relation(
        STAGING,
        "PATCHES",
        &web,
        (PATCH, 0),
        json!({
            "path": PATCH,
            "replicas": 2,
            "containers": [
                {"name": "web", "image": "shop/web:2-rc", "environment_keys": ["LOG_LEVEL"]},
            ],
            "more": [{"path": "k8s/overlays/staging/replicas.yaml", "replicas": 3}],
        }),
    );
    f.kustomization(
        PROD,
        json!({
            "includes": [BASE],
            "image_overrides": [{"name": "shop/other", "newTag": "9"}],
        }),
    );
    f.manifest(
        DEPLOYMENT,
        (0, "Deployment", "web"),
        json!({
            "replicas": 1,
            "containers": [
                {"init": true, "name": "migrate", "image": "shop/migrate:1"},
                {
                    "name": "web",
                    "image": "registry.example.com:5000/shop/web:1",
                    "ports": ["8080"],
                    "environment_keys": ["DATABASE_URL", "SESSION_KEY"],
                    "env_from": ["ConfigMap/web-config", "Secret/web-secrets"],
                },
            ],
        }),
    );
    f.relation(PROD, "DECLARES", &web, (PROD, 20), json!({"via": [BASE]}));
    f.kustomization(BASE, json!({}));
    f.relation(BASE, "DECLARES", &web, (BASE, 14), json!({"via": []}));
    f.kustomization(
        STAGING,
        json!({
            "name": "shop-staging",
            "name_span": [0, 23],
            "includes": [BASE],
            "image_overrides": [
                {"name": "registry.example.com:5000/shop/web", "newTag": "2", "newName": "mirror/web"},
                {"name": "shop/unrelated", "newTag": "7"},
            ],
        }),
    );
    f.relation(
        STAGING,
        "DECLARES",
        &web,
        (STAGING, 40),
        json!({"via": [BASE]}),
    );
    f.manifest(
        "jobs/seed.yaml",
        (1, "Job", "seed"),
        json!({"namespace": "tools"}),
    );
    f.gap(
        SHOP,
        PROD,
        ("kustomization", None),
        "`namePrefix` is present and not evaluated",
        &[(60, 70)],
    );
    f.fact(&format!("gap:environment:{SHOP}@{PROD}")).props["unevaluated"] = json!(["namePrefix"]);
    let id = format!("gap:environment:{SHOP}@jobs/seed.yaml#1");
    f.nodes.push(Node {
        props: json!({
            "kind": "manifest-document",
            "path": "jobs/seed.yaml",
            "document": 1,
            "workload": "Job/seed",
            "reason": "`spec.template.spec.containers` is not a sequence of mappings",
            "attempted_tiers": ["T0"],
            "prov": prov(Tier::Deterministic, ConfidenceTier::Gap, SHOP, "jobs/seed.yaml", &[(90, 200)], &id),
        }),
        id,
        label: "Gap".into(),
    });
    f
}

/// The rows of the `nth` table under `## heading`, from 0.
fn nth_table<'a>(content: &'a str, heading: &str, nth: usize) -> Vec<&'a str> {
    let start = content
        .find(&format!("\n## {heading}\n"))
        .unwrap_or_else(|| panic!("no section {heading} in:\n{content}"));
    let section = &content[start + 1..];
    let section = &section[..section[3..]
        .find("\n## ")
        .map_or(section.len(), |end| end + 3)];
    let mut tables: Vec<Vec<&str>> = Vec::new();
    let mut open = false;
    for line in section.lines() {
        if line.starts_with("|---") {
            tables.push(Vec::new());
            open = true;
        } else if open && line.starts_with('|') {
            tables.last_mut().unwrap().push(line);
        } else {
            open = false;
        }
    }
    tables.swap_remove(nth)
}

const WEB_SPAN: &str = "Deterministic | Confirmed | acme/shop:k8s/base/deployment.yaml bytes 0..400 \
                        @ 0123abc";
const RENAMES: &str =
    "this environment sets `namePrefix`: the deployed name differs from the recorded one";

// AC-0662: each kustomization is an environment shown by its path and named
// only where it states a namespace; one that another includes says which
// include it, and one that sets `namePrefix` says the deployed name differs.
#[test]
fn kustomizations_are_environments_and_an_included_one_says_what_includes_it() {
    let f = overlays();
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let content = f.artifact(mode).content;
        assert!(content.contains(
            "Environments: 3. Workloads: 2. Environments named only by a file name: 0. Other \
             items not established: 2. Workloads no environment declares: 1."
        ));
        assert_eq!(
            table(&content, "Environments"),
            [
                "| acme/shop | k8s/base/kustomization.yaml | none stated | kustomize | 1 | \
                 Deterministic | Confirmed | acme/shop:k8s/base/kustomization.yaml bytes 0..9 @ \
                 0123abc | included by k8s/overlays/prod/kustomization.yaml, \
                 k8s/overlays/staging/kustomization.yaml |"
                    .to_string(),
                format!(
                    "| acme/shop | k8s/overlays/prod/kustomization.yaml | none stated | kustomize \
                     | 1 | Deterministic | Confirmed | \
                     acme/shop:k8s/overlays/prod/kustomization.yaml bytes 0..9 @ 0123abc | \
                     {RENAMES} |"
                ),
                "| acme/shop | k8s/overlays/staging/kustomization.yaml | shop-staging | kustomize \
                 | 1 | Deterministic | Confirmed | \
                 acme/shop:k8s/overlays/staging/kustomization.yaml bytes 0..9 @ 0123abc | — |"
                    .to_string(),
            ]
        );
        // Nothing is named from a path.
        assert!(!content.contains("| prod |"));
        assert!(!content.contains("| staging |"));
        assert!(!content.contains("| base |"));
    }
}

// AC-0662: a manifest workload has one row per container under each
// environment that reaches it, with the chain it is reached through; each
// cell is what the manifest writes, whatever the environment patches.
#[test]
fn a_manifest_workload_has_one_row_per_container_under_each_environment() {
    let content = overlays().artifact(ExportMode::BestEffort).content;
    // No Compose service: the first table says so.
    assert_eq!(table(&content, "Workloads by environment").len(), 1);
    let rows = nth_table(&content, "Workloads by environment", 1);
    assert_eq!(rows.len(), 3 * 2);
    assert_eq!(
        rows[0],
        format!(
            "| acme/shop | k8s/base/kustomization.yaml | its own entry | web | Deployment | \
             k8s/base/deployment.yaml | — | 1 | migrate (init container) | shop/migrate:1 | — | — \
             | — | {WEB_SPAN} | — |"
        )
    );
    assert_eq!(
        rows[3],
        format!(
            "| acme/shop | k8s/overlays/prod/kustomization.yaml | k8s/base/kustomization.yaml | \
             web | Deployment | k8s/base/deployment.yaml | — | 1 | web | \
             registry.example.com:5000/shop/web:1 | 8080 | DATABASE\\_URL, SESSION\\_KEY | \
             ConfigMap/web-config, Secret/web-secrets | {WEB_SPAN} | {RENAMES} |"
        )
    );
    // Staging patches the image and the replicas; its rows still show what
    // the manifest writes.
    assert_eq!(
        rows[5],
        format!(
            "| acme/shop | k8s/overlays/staging/kustomization.yaml | k8s/base/kustomization.yaml \
             | web | Deployment | k8s/base/deployment.yaml | — | 1 | web | \
             registry.example.com:5000/shop/web:1 | 8080 | DATABASE\\_URL, SESSION\\_KEY | \
             ConfigMap/web-config, Secret/web-secrets | {WEB_SPAN} | — |"
        )
    );
}

// AC-0662 (ADR-0070 decisions 6 and 8): a workload that several environments
// reach is listed once per environment with what that environment's patch
// files and `images` entries state for it, or "no patch stated". Nothing is
// merged, and the document never says which declaration applies.
#[test]
fn a_workload_several_environments_reach_is_listed_once_per_environment_with_its_patch() {
    let f = overlays();
    let content = f.artifact(ExportMode::BestEffort).content;
    // One fact under one name: the table of differing declarations is empty.
    let by_name = table(&content, "Declared in more than one environment");
    assert_eq!(by_name.len(), 1);
    assert!(by_name[0].contains("None in this export"));
    assert_eq!(
        nth_table(&content, "Declared in more than one environment", 1),
        [
            "| acme/shop | web | Deployment | k8s/base/deployment.yaml | \
             k8s/base/kustomization.yaml | its own entry | no patch stated | none names its \
             images | Deterministic | Confirmed | acme/shop:k8s/base/kustomization.yaml bytes \
             14..24 @ 0123abc | — |"
                .to_string(),
            format!(
                "| acme/shop | web | Deployment | k8s/base/deployment.yaml | \
                 k8s/overlays/prod/kustomization.yaml | k8s/base/kustomization.yaml | no patch \
                 stated | none names its images | Deterministic | Confirmed | \
                 acme/shop:k8s/overlays/prod/kustomization.yaml bytes 20..30 @ 0123abc | \
                 {RENAMES} |"
            ),
            "| acme/shop | web | Deployment | k8s/base/deployment.yaml | \
             k8s/overlays/staging/kustomization.yaml | k8s/base/kustomization.yaml | patch \
             k8s/overlays/staging/patch.yaml states replicas 2; container web: image \
             shop/web:2-rc, environment key names LOG\\_LEVEL / patch \
             k8s/overlays/staging/replicas.yaml states replicas 3 | name \
             registry.example.com:5000/shop/web, newName mirror/web, newTag 2 | Deterministic | \
             Confirmed | acme/shop:k8s/overlays/staging/kustomization.yaml bytes 40..50 @ 0123abc \
             | — |"
                .to_string(),
        ]
    );
    // An `images` entry that names none of its images is not shown for it.
    assert!(!content.contains("shop/unrelated"));
    assert!(!content.contains("shop/other"));
    let lower = content.to_lowercase();
    for word in [
        "overrid",
        "takes precedence",
        "effective image",
        "effective replicas",
        "is applied to",
    ] {
        assert!(!lower.contains(word), "{word}");
    }
    assert!(content.contains("this section does not say which declaration applies where."));
}

// AC-0662: two kustomizations that each declare their own workload of one
// name are two declarations, listed with the properties that differ.
#[test]
fn two_manifest_workloads_of_one_name_are_listed_with_what_differs() {
    let mut f = Fixture::default();
    for (dir, image, replicas) in [("a", "api:1", 1), ("b", "api:2", 4)] {
        let kustomization = format!("{dir}/kustomization.yaml");
        let file = format!("{dir}/api.yaml");
        f.kustomization(&kustomization, json!({}));
        f.manifest(
            &file,
            (0, "Deployment", "api"),
            json!({"replicas": replicas, "containers": [{"name": "api", "image": image}]}),
        );
        f.relation(
            &kustomization,
            "DECLARES",
            &manifest_id(&file, 0, "Deployment", "api"),
            (&kustomization, 12),
            json!({"via": []}),
        );
    }
    let content = f.artifact(ExportMode::VerifiedOnly).content;
    assert_eq!(
        table(&content, "Declared in more than one environment"),
        [
            "| acme/shop | api | a/kustomization.yaml | replicas: 1; containers: container api: \
             image api:1 | Deterministic | Confirmed | acme/shop:a/api.yaml bytes 0..400 @ \
             0123abc | — |",
            "| acme/shop | api | b/kustomization.yaml | replicas: 4; containers: container api: \
             image api:2 | Deterministic | Confirmed | acme/shop:b/api.yaml bytes 0..400 @ \
             0123abc | — |",
        ]
    );
    // Neither is reached by two environments.
    assert!(
        nth_table(&content, "Declared in more than one environment", 1)[0]
            .contains("None in this export")
    );
}

// AC-0662: a manifest workload with no `DECLARES` relation is listed under
// "Workloads no environment declares", and the Gaps of a kustomization and
// of a manifest document are listed with their reasons.
#[test]
fn workloads_no_environment_declares_and_the_new_gaps_are_listed() {
    let f = overlays();
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let content = f.artifact(mode).content;
        assert_eq!(
            table(&content, "Workloads no environment declares"),
            [
                "| acme/shop | jobs/seed.yaml | seed | Job | tools | — | none recorded | — | — | — \
                 | — | Deterministic | Confirmed | acme/shop:jobs/seed.yaml bytes 0..400 @ \
                 0123abc | — |"
            ]
        );
        // The reached workload is not there.
        assert!(
            !content[content
                .find("## Workloads no environment declares")
                .unwrap()..content.find("## Not established").unwrap()]
                .contains("| web |")
        );
        assert_eq!(
            table(&content, "Not established"),
            [
                "| acme/shop | jobs/seed.yaml | Job/seed | Kubernetes manifest document | \
                 \\`spec.template.spec.containers\\` is not a sequence of mappings | \
                 Deterministic | Gap | acme/shop:jobs/seed.yaml bytes 90..200 @ 0123abc |",
                "| acme/shop | k8s/overlays/prod/kustomization.yaml | — | kustomization | \
                 \\`namePrefix\\` is present and not evaluated | Deterministic | Gap | \
                 acme/shop:k8s/overlays/prod/kustomization.yaml bytes 60..70 @ 0123abc |",
            ]
        );
        for stated in [
            "only `Deployment`, `StatefulSet`, `DaemonSet` and `Job` documents are read",
            "Helm charts and Helm values files are not read",
            "Nothing is applied",
            "at most 16 kustomizations deep",
            "`valueFrom` and the `data` of any document are not read",
        ] {
            assert!(content.contains(stated), "{stated}");
        }
    }
    // Without the facts the section still renders.
    let empty = Fixture::default().artifact(ExportMode::BestEffort).content;
    for (heading, nth) in [
        ("Workloads by environment", 1),
        ("Declared in more than one environment", 1),
        ("Workloads no environment declares", 0),
    ] {
        let rows = nth_table(&empty, heading, nth);
        assert_eq!(rows.len(), 1, "{heading}");
        assert!(rows[0].contains("None in this export"));
    }
}

// AC-0662: the same graph yields the same bytes whatever order the facts
// arrive in, the selected compiler agrees, and every assertion has a row in
// the fact index that names this artifact.
#[test]
fn the_kustomize_document_is_deterministic_and_every_row_cites_an_indexed_fact() {
    let f = overlays();
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let first = f.artifact(mode);
        assert_eq!(f.artifact(mode), first);
        let mut reversed = Fixture {
            nodes: f.nodes.iter().rev().cloned().collect(),
            edges: f.edges.iter().rev().cloned().collect(),
        };
        reversed.nodes.extend(f.nodes.iter().take(2).cloned());
        reversed.edges.extend(f.edges.iter().take(2).cloned());
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
        // 3 environments, 2 workloads, 3 DECLARES, 1 PATCHES and 2 Gaps.
        assert_eq!(artifact.assertions.len(), 3 + 2 + 3 + 1 + 2);
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
            assert!(row.cited_by.iter().any(|by| by == ID));
            assert!(row.records.iter().any(|record| {
                record.tier == assertion.provenance.tier
                    && record.confidence == assertion.provenance.confidence_tier
                    && record.evidence == assertion.provenance.evidence
            }));
            cited.push(row.citation_id.clone());
        }
        check_citations(&index, cited.iter().map(String::as_str)).expect("every id resolves");
        // The patch relation is cited where the patch is written.
        assert!(artifact.assertions.iter().any(|assertion| {
            assertion.subject_id.contains(" PATCHES ")
                && assertion.provenance.evidence[0].path == PATCH
        }));
    }
}
