//! Kubernetes manifests and kustomize (#1791, AC-0662, SPEC-14 increment 2).
use super::*;

const KUSTOMIZATION: &str = "apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\n";

fn deployment() -> String {
    format!(
        r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  labels:
    owner: {SECRET}
spec:
  replicas: 1
  template:
    spec:
      initContainers:
        - name: migrate
          image: shop/migrate:1
          command: ["migrate", "--password", "{SECRET}"]
      containers:
        - name: web
          image: shop/web:1
          ports:
            - containerPort: 8080
              name: http
            - containerPort: 9090
          env:
            - name: SESSION_KEY
              value: {SECRET}
            - name: DATABASE_URL
              valueFrom:
                secretKeyRef:
                  name: db-{SECRET}
                  key: url
          envFrom:
            - configMapRef:
                name: web-config
            - secretRef:
                name: web-secrets
        - name: sidecar
          image: shop/proxy:3
"#
    )
}

fn staging_patch() -> String {
    format!(
        r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
spec:
  replicas: 2
  template:
    spec:
      containers:
        - name: web
          image: shop/web:2-rc
          env:
            - name: LOG_LEVEL
              value: {SECRET}
"#
    )
}

const STAGING: &str = "namespace: shop-staging\nresources:\n  - ../../base\npatches:\n  - path: \
                       patch.yaml\nimages:\n  - name: shop/web\n    newTag: \"2\"\n";
const PROD: &str = "resources:\n  - ../../base\npatchesStrategicMerge:\n  - patch.yaml\nimages:\n  \
                    - name: shop/web\n    newName: registry.example.com/web\n    newTag: 2.0.1\n";
const PROD_PATCH: &str = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\nspec:\n  \
                          replicas: 5\n";

fn overlays() -> Vec<(&'static str, String)> {
    vec![
        (
            "base/kustomization.yaml",
            format!("{KUSTOMIZATION}resources:\n  - deployment.yaml\n"),
        ),
        ("base/deployment.yaml", deployment()),
        ("overlays/staging/kustomization.yaml", STAGING.to_string()),
        ("overlays/staging/patch.yaml", staging_patch()),
        ("overlays/prod/kustomization.yaml", PROD.to_string()),
        ("overlays/prod/patch.yaml", PROD_PATCH.to_string()),
    ]
}

fn extract_owned(files: &[(&str, String)]) -> EnvironmentExtraction {
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, body)| (*path, body.as_str()))
        .collect();
    extract(&files)
}

const WEB: &str = "workload:local/app@base/deployment.yaml#0:Deployment/web";

fn labelled<'a>(out: &'a EnvironmentExtraction, label: &str) -> Vec<&'a str> {
    out.nodes
        .iter()
        .filter(|node| node.label == label)
        .map(|node| node.id.as_str())
        .collect()
}

/// The one Gap of the kustomization at `path`.
fn kustomization_gap<'a>(out: &'a EnvironmentExtraction, path: &str) -> &'a Node {
    let gap = node(out, &format!("gap:environment:local/app@{path}"));
    assert_eq!(gap.label, "Gap");
    assert_eq!(gap.props["kind"], "kustomization");
    assert_eq!(gap.props["path"], path);
    assert_eq!(gap.props["attempted_tiers"], serde_json::json!(["T0"]));
    assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");
    assert_eq!(gap.props["prov"]["tier"], "Deterministic");
    gap
}

fn reasons(gap: &Node) -> Vec<&str> {
    gap.props["reason"].as_str().unwrap().split("; ").collect()
}

// AC-0662: each `Deployment`, `StatefulSet`, `DaemonSet` and `Job` document
// is a Workload identified by its file, its position in the file and its
// kind and name, with its containers and init containers in file order; a
// namespace only when stated and replicas only when a literal integer. Each
// is T0 Confirmed and cites its document.
#[test]
fn each_pod_bearing_kind_is_a_workload_with_its_containers() {
    let text = format!(
        "{}---\napiVersion: apps/v1\nkind: StatefulSet\nmetadata:\n  name: db\n  namespace: \
         data\nspec:\n  replicas: \"3\"\n  template:\n    spec:\n      containers:\n        - \
         name: postgres\n          image: postgres:16\n---\napiVersion: v1\nkind: Service\n\
         metadata:\n  name: web\n---\napiVersion: apps/v1\nkind: DaemonSet\nmetadata:\n  name: \
         agent\nspec:\n  template:\n    spec:\n      containers:\n        - name: agent\n          \
         image: shop/agent:1\n---\napiVersion: batch/v1\nkind: Job\nmetadata:\n  name: seed\n\
         spec:\n  template:\n    spec:\n      containers:\n        - image: shop/seed:1\n",
        deployment()
    );
    let out = extract(&[("k8s/all.yaml", &text)]);
    assert_eq!(out.files, 1);
    assert!(gaps(&out).is_empty(), "{:?}", gaps(&out));
    assert!(out.edges.is_empty());
    // The Service is document 2 and is not read.
    assert_eq!(
        ids(&out),
        [
            "workload:local/app@k8s/all.yaml#0:Deployment/web",
            "workload:local/app@k8s/all.yaml#1:StatefulSet/db",
            "workload:local/app@k8s/all.yaml#3:DaemonSet/agent",
            "workload:local/app@k8s/all.yaml#4:Job/seed",
        ]
    );
    for workload in &out.nodes {
        assert_eq!(workload.label, "Workload");
        assert_eq!(workload.props["path"], "k8s/all.yaml");
        assert_eq!(workload.props["prov"]["tier"], "Deterministic");
        assert_eq!(workload.props["prov"]["confidence_tier"], "Confirmed");
        assert_eq!(
            workload.props["prov"]["extractor_id"],
            ENVIRONMENT_EXTRACTOR_ID
        );
    }

    let web = node(&out, "workload:local/app@k8s/all.yaml#0:Deployment/web");
    assert_eq!(web.props["kind"], "k8s-deployment");
    assert_eq!(web.props["name"], "web");
    assert_eq!(web.props["replicas"], 1);
    assert!(web.props.get("namespace").is_none());
    // In file order: the init container is written first here.
    assert_eq!(
        web.props["containers"],
        serde_json::json!([
            {"init": true, "name": "migrate", "image": "shop/migrate:1"},
            {
                "name": "web",
                "image": "shop/web:1",
                "ports": ["8080", "9090"],
                "environment_keys": ["DATABASE_URL", "SESSION_KEY"],
                "env_from": ["ConfigMap/web-config", "Secret/web-secrets"],
            },
            {"name": "sidecar", "image": "shop/proxy:3"},
        ])
    );
    let span = cited(&web.props, &text);
    assert_eq!(span.len(), 1);
    assert!(span[0].starts_with("apiVersion: apps/v1\nkind: Deployment"));
    assert!(span[0].trim_end().ends_with("image: shop/proxy:3"));

    let db = node(&out, "workload:local/app@k8s/all.yaml#1:StatefulSet/db");
    assert_eq!(db.props["kind"], "k8s-statefulset");
    assert_eq!(db.props["namespace"], "data");
    // A quoted number is not a literal integer.
    assert!(db.props.get("replicas").is_none());
    assert!(cited(&db.props, &text)[0].contains("kind: StatefulSet"));
    assert!(!cited(&db.props, &text)[0].contains("kind: Service"));

    let agent = node(&out, "workload:local/app@k8s/all.yaml#3:DaemonSet/agent");
    assert_eq!(agent.props["kind"], "k8s-daemonset");
    assert!(agent.props.get("replicas").is_none());
    let seed = node(&out, "workload:local/app@k8s/all.yaml#4:Job/seed");
    assert_eq!(seed.props["kind"], "k8s-job");
    assert_eq!(
        seed.props["containers"],
        serde_json::json!([{"image": "shop/seed:1"}])
    );
}

// AC-0662 (ADR-0070 decision 5): no `env[].value`, no `valueFrom`, no `data`
// or `stringData` and no generator literal reaches a fact, and a
// token-shaped image is withheld with the count disclosed.
#[test]
fn no_value_of_a_manifest_or_a_kustomization_is_stored() {
    let secret = format!(
        "apiVersion: v1\nkind: Secret\nmetadata:\n  name: web-secrets\ndata:\n  password: \
         {SECRET}\nstringData:\n  token: {SECRET}\n---\napiVersion: v1\nkind: ConfigMap\n\
         metadata:\n  name: web-config\ndata:\n  url: http://{SECRET}\n"
    );
    let generators = format!(
        "{KUSTOMIZATION}resources:\n  - deployment.yaml\n  - secret.yaml\nsecretGenerator:\n  - \
         name: db\n    literals:\n      - password={SECRET}\nconfigMapGenerator:\n  - name: \
         app\n    literals:\n      - URL=http://{SECRET}\npatches:\n  - patch: |-\n      - op: \
         replace\n        path: /spec/replicas\n        value: {SECRET}\n"
    );
    let leaky = format!(
        "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: leaky\nspec:\n  template:\n    \
         spec:\n      containers:\n        - name: app\n          image: \
         registry.example.com/{TOKEN}:1\n          args: [\"--key\", \"{SECRET}\"]\n"
    );
    let mut files = overlays();
    files[0].1 = generators;
    files.push(("base/secret.yaml", secret));
    files.push(("base/leaky.yaml", leaky));
    let out = extract_owned(&files);
    let stored = serialized(&out);
    assert!(files.iter().any(|(_, body)| body.contains(SECRET)));
    assert!(!stored.contains(SECRET), "{stored}");
    assert!(!stored.contains(TOKEN));
    assert!(!stored.contains("secretKeyRef"));
    assert!(!stored.contains("password"));
    // The names a container references are stored; their content is not.
    assert!(stored.contains("Secret/web-secrets"));
    let leaky = node(
        &out,
        "workload:local/app@base/leaky.yaml#0:Deployment/leaky",
    );
    assert_eq!(
        leaky.props["containers"],
        serde_json::json!([{"name": "app", "image": "[WITHHELD]:1", "redactions": 1}])
    );
    // The generators are named by their keys alone.
    let base = "base/kustomization.yaml";
    let gap = kustomization_gap(&out, base);
    assert_eq!(
        reasons(gap),
        [
            "an inline patch is not read",
            "`configMapGenerator` is present and not evaluated",
            "`secretGenerator` is present and not evaluated",
        ]
    );
    assert_eq!(
        gap.props["unevaluated"],
        serde_json::json!(["configMapGenerator", "secretGenerator"])
    );
    let mut spans = cited(&gap.props, &files[0].1);
    spans.sort_unstable();
    assert_eq!(spans, ["configMapGenerator", "patch", "secretGenerator"]);
}

// AC-0662 (ADR-0070 decision 8): a base and two overlays are three
// Environments. Each declares the base workload, the overlays through the
// base; each overlay's patch file is a `PATCHES` relation stating only what
// the patch writes; an environment is named only where its file states a
// `namespace`, and nothing is named from a path.
#[test]
fn a_base_and_two_overlays_are_three_environments_declaring_one_workload() {
    let files = overlays();
    let out = extract_owned(&files);
    assert_eq!(out.files, 4);
    assert!(gaps(&out).is_empty(), "{:?}", gaps(&out));
    let base = "env:local/app@base/kustomization.yaml";
    let prod = "env:local/app@overlays/prod/kustomization.yaml";
    let staging = "env:local/app@overlays/staging/kustomization.yaml";
    assert_eq!(labelled(&out, "Environment"), [base, prod, staging]);
    // One workload: a patch file is read only as a patch.
    assert_eq!(labelled(&out, "Workload"), [WEB]);

    let environment = node(&out, base);
    assert_eq!(environment.props["declared_by"], "kustomize");
    assert_eq!(environment.props["path"], "base/kustomization.yaml");
    for absent in ["name", "name_span", "includes", "image_overrides"] {
        assert!(environment.props.get(absent).is_none(), "{absent}");
    }
    assert_eq!(cited(&environment.props, &files[0].1), ["resources"]);

    let environment = node(&out, staging);
    assert_eq!(environment.props["name"], "shop-staging");
    let span = &environment.props["name_span"];
    assert_eq!(
        &STAGING[span[0].as_u64().unwrap() as usize..span[1].as_u64().unwrap() as usize],
        "namespace: shop-staging"
    );
    assert_eq!(
        environment.props["includes"],
        serde_json::json!(["base/kustomization.yaml"])
    );
    assert_eq!(
        environment.props["image_overrides"],
        serde_json::json!([{"name": "shop/web", "newTag": "2"}])
    );
    assert_eq!(cited(&environment.props, STAGING), ["resources"]);

    // Nothing is named from `overlays/prod`.
    let environment = node(&out, prod);
    assert!(environment.props.get("name").is_none());
    assert!(environment.props.get("name_span").is_none());
    assert_eq!(
        environment.props["image_overrides"],
        serde_json::json!([
            {"name": "shop/web", "newName": "registry.example.com/web", "newTag": "2.0.1"}
        ])
    );
    let stored = serialized(&out);
    assert!(!stored.contains("\"prod\""));
    assert!(!stored.contains("\"staging\""));

    // DECLARES from each, citing the entry of its own file that leads there.
    let declares = edge(&out, base, "DECLARES", WEB);
    assert_eq!(declares.props["via"], serde_json::json!([]));
    assert_eq!(cited(&declares.props, &files[0].1), ["deployment.yaml"]);
    for (environment, text) in [(staging, STAGING), (prod, PROD)] {
        let declares = edge(&out, environment, "DECLARES", WEB);
        assert_eq!(
            declares.props["via"],
            serde_json::json!(["base/kustomization.yaml"])
        );
        assert_eq!(cited(&declares.props, text), ["../../base"]);
        assert_eq!(declares.props["prov"]["confidence_tier"], "Confirmed");
    }

    // PATCHES: what each patch writes, cited where the patch is written.
    let patches = edge(&out, staging, "PATCHES", WEB);
    assert_eq!(patches.props["path"], "overlays/staging/patch.yaml");
    assert_eq!(patches.props["replicas"], 2);
    assert_eq!(
        patches.props["containers"],
        serde_json::json!([
            {"name": "web", "image": "shop/web:2-rc", "environment_keys": ["LOG_LEVEL"]}
        ])
    );
    assert_eq!(
        patches.props["prov"]["evidence"][0]["path"],
        "overlays/staging/patch.yaml"
    );
    let patch = staging_patch();
    assert_eq!(cited(&patches.props, &patch), [patch.as_str()]);
    let patches = edge(&out, prod, "PATCHES", WEB);
    assert_eq!(patches.props["path"], "overlays/prod/patch.yaml");
    assert_eq!(patches.props["replicas"], 5);
    assert!(patches.props.get("containers").is_none());
    assert!(patches.props.get("more").is_none());
    // The base workload keeps its own properties: nothing is merged.
    let web = node(&out, WEB);
    assert_eq!(web.props["replicas"], 1);
    assert_eq!(web.props["containers"][1]["image"], "shop/web:1");
    assert_eq!(out.edges.len(), 3 + 2);
    assert!(
        out.edges
            .iter()
            .all(|edge| edge.label != "PATCHES" || edge.src != base)
    );
}

// AC-0662: two patch documents of one kustomization for one workload are
// one relation; the first gives its properties and the second is listed
// after it, each cited.
#[test]
fn two_patch_documents_for_one_workload_are_one_relation() {
    let mut files = overlays();
    files[2].1 = "resources:\n  - ../../base\npatches:\n  - path: patch.yaml\n  - path: \
                  replicas.yaml\n"
        .to_string();
    files.push(("overlays/staging/replicas.yaml", PROD_PATCH.to_string()));
    let out = extract_owned(&files);
    assert!(gaps(&out).is_empty(), "{:?}", gaps(&out));
    let patches = edge(
        &out,
        "env:local/app@overlays/staging/kustomization.yaml",
        "PATCHES",
        WEB,
    );
    assert_eq!(patches.props["path"], "overlays/staging/patch.yaml");
    assert_eq!(patches.props["replicas"], 2);
    assert_eq!(
        patches.props["more"],
        serde_json::json!([{"path": "overlays/staging/replicas.yaml", "replicas": 5}])
    );
    let paths: Vec<&str> = patches.props["prov"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|span| span["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            "overlays/staging/patch.yaml",
            "overlays/staging/replicas.yaml"
        ]
    );
}

// AC-0662: an entry that is remote, leaves the repository or names nothing
// the walk found is stated by the kustomization's one Gap, each with the
// span of its entry; what the other entries reach is still declared.
#[test]
fn remote_escaping_and_missing_entries_are_gaps() {
    let kustomization = "resources:\n  - deployment.yaml\n  - \
                         https://github.com/example/infra//base?ref=v1\n  - \
                         github.com/example/other\n  - ../../outside\n  - /etc/manifests\n  - \
                         missing.yaml\n  - empty\n  - notes.yaml\n  - broken.yaml\nbases:\n  - \
                         ../gone\npatches:\n  - path: ../../../patch.yaml\n  - path: \
                         absent-patch.yaml\n";
    let out = extract_owned(&[
        ("deploy/kustomization.yaml", kustomization.to_string()),
        ("deploy/deployment.yaml", deployment()),
        ("deploy/empty/README.md", "nothing here\n".to_string()),
        ("deploy/notes.yaml", "title: notes\n".to_string()),
        (
            "deploy/broken.yaml",
            "apiVersion: v1\nkind: [unclosed\n".to_string(),
        ),
    ]);
    let environment = "env:local/app@deploy/kustomization.yaml";
    assert_eq!(
        edge(
            &out,
            environment,
            "DECLARES",
            "workload:local/app@deploy/deployment.yaml#0:Deployment/web"
        )
        .props["via"],
        serde_json::json!([])
    );
    assert_eq!(out.edges.len(), 1);
    let gap = kustomization_gap(&out, "deploy/kustomization.yaml");
    assert_eq!(
        reasons(gap),
        [
            "`resources` entry `https://github.com/example/infra//base?ref=v1` is a remote \
             reference, which is not fetched",
            "`resources` entry `github.com/example/other` is a remote \
             reference, which is not fetched",
            "`resources` entry `../../outside` is a path outside the repository",
            "`resources` entry `/etc/manifests` is a path outside the repository",
            "`resources` entry `missing.yaml` names no file or kustomization directory among \
             the files read",
            "`resources` entry `empty` names a directory that holds no kustomization file",
            "`resources` entry `notes.yaml` names a file that holds no manifest document",
            "`resources` entry `broken.yaml` names a file that is not valid YAML",
            "`bases` entry `../gone` names no file or kustomization directory among the files \
             read",
            "`patches` entry `../../../patch.yaml` is a path outside the repository",
            "`patches` entry `absent-patch.yaml` names no file or kustomization directory among \
             the files read",
        ]
    );
    let mut spans: Vec<&str> = cited(&gap.props, kustomization)
        .into_iter()
        .map(str::trim_end)
        .collect();
    spans.sort_unstable();
    assert_eq!(
        spans,
        [
            "../../outside",
            "../gone",
            "/etc/manifests",
            "broken.yaml",
            "empty",
            "github.com/example/other",
            "https://github.com/example/infra//base?ref=v1",
            "missing.yaml",
            "notes.yaml",
            "path: ../../../patch.yaml",
            "path: absent-patch.yaml",
        ]
    );
    assert!(gap.props.get("unevaluated").is_none());
}

// AC-0662: a patch document that matches no reached workload, an inline
// patch, a `target` selector and `patchesJson6902` are stated by the Gap,
// and none of them yields a `PATCHES` relation.
#[test]
fn unmatched_inline_selected_and_json_patches_are_gaps() {
    let kustomization = "resources:\n  - deployment.yaml\npatches:\n  - path: other.yaml\n  - \
                         patch: |-\n      kind: Deployment\n  - path: selected.yaml\n    \
                         target:\n      kind: Deployment\npatchesStrategicMerge:\n  - |-\n    \
                         kind: Deployment\npatchesJson6902:\n  - path: ops.yaml\n";
    let other = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: api\nspec:\n  \
                 replicas: 3\n---\napiVersion: v1\nkind: Service\nmetadata:\n  name: web\n---\n\
                 apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  labels: {}\n";
    let out = extract_owned(&[
        ("kustomization.yaml", kustomization.to_string()),
        ("deployment.yaml", deployment()),
        ("other.yaml", other.to_string()),
        ("selected.yaml", PROD_PATCH.to_string()),
    ]);
    assert!(out.edges.iter().all(|edge| edge.label != "PATCHES"));
    // The patch files yield no workload of their own.
    assert_eq!(
        labelled(&out, "Workload"),
        ["workload:local/app@deployment.yaml#0:Deployment/web"]
    );
    let gap = kustomization_gap(&out, "kustomization.yaml");
    assert_eq!(
        reasons(gap),
        [
            "an inline patch is not read",
            "a `patches` entry with a `target` selector is not evaluated",
            "`patchesJson6902` is not read",
            "patch document `Deployment/api` in `other.yaml` matches no workload this \
             environment reaches",
            "a patch document in `other.yaml` is a `Service`, a kind that is not read",
            "a patch document in `other.yaml` has no plain scalar `metadata.name`",
        ]
    );
    let mut spans = cited(&gap.props, kustomization);
    spans.sort_unstable();
    assert_eq!(
        spans,
        [
            "patch",
            "patchesJson6902",
            "path: other.yaml",
            "target",
            "|-\n    kind: Deployment"
        ]
    );
}

// AC-0662: each transformer and generator key that is present is named by
// the Gap and listed on it; none is applied.
#[test]
fn every_transformer_and_generator_key_is_named_and_none_is_applied() {
    let keys = [
        "namePrefix",
        "nameSuffix",
        "commonLabels",
        "labels",
        "replicas",
        "configMapGenerator",
        "secretGenerator",
        "components",
        "helmCharts",
        "generators",
        "transformers",
        "replacements",
        "vars",
    ];
    let mut kustomization = String::from("resources:\n  - deployment.yaml\n");
    for key in keys {
        kustomization.push_str(&format!("{key}: prod-\n"));
    }
    let out = extract_owned(&[
        ("kustomization.yaml", kustomization.clone()),
        ("deployment.yaml", deployment()),
    ]);
    let gap = kustomization_gap(&out, "kustomization.yaml");
    let expected: Vec<String> = keys
        .iter()
        .map(|key| format!("`{key}` is present and not evaluated"))
        .collect();
    assert_eq!(reasons(gap), expected);
    assert_eq!(gap.props["unevaluated"], serde_json::json!(keys));
    let mut spans = cited(&gap.props, &kustomization);
    spans.sort_unstable();
    let mut sorted = keys;
    sorted.sort_unstable();
    assert_eq!(spans, sorted);
    // The recorded name and replicas are the file's own.
    let web = node(&out, "workload:local/app@deployment.yaml#0:Deployment/web");
    assert_eq!(web.props["name"], "web");
    assert_eq!(web.props["replicas"], 1);
    assert!(!serialized(&out).contains("prod-"));
}

// AC-0662: a cycle ends the walk on that branch with a Gap, and a chain is
// followed 16 kustomizations deep with a Gap beyond it.
#[test]
fn a_cycle_and_a_chain_deeper_than_the_bound_are_gaps() {
    let out = extract_owned(&[
        (
            "a/kustomization.yaml",
            "resources:\n  - ../b\n  - deployment.yaml\n".to_string(),
        ),
        ("a/deployment.yaml", deployment()),
        ("b/kustomization.yml", "resources:\n  - ../a\n".to_string()),
        ("self/Kustomization", "resources:\n  - .\n".to_string()),
    ]);
    let web = "workload:local/app@a/deployment.yaml#0:Deployment/web";
    assert_eq!(
        reasons(kustomization_gap(&out, "a/kustomization.yaml")),
        [
            "the `resources` chain returns to `a/kustomization.yaml`, a cycle, and is not followed further"
        ]
    );
    assert_eq!(
        reasons(kustomization_gap(&out, "b/kustomization.yml")),
        [
            "the `resources` chain returns to `b/kustomization.yml`, a cycle, and is not followed further"
        ]
    );
    assert_eq!(
        reasons(kustomization_gap(&out, "self/Kustomization")),
        [
            "the `resources` chain returns to `self/Kustomization`, a cycle, and is not followed further"
        ]
    );
    // What was reached before the cycle closed is still declared.
    assert_eq!(
        edge(&out, "env:local/app@a/kustomization.yaml", "DECLARES", web).props["via"],
        serde_json::json!([])
    );
    assert_eq!(
        edge(&out, "env:local/app@b/kustomization.yml", "DECLARES", web).props["via"],
        serde_json::json!(["a/kustomization.yaml"])
    );

    // k00 -> k01 -> ... -> k18, the workload beside k18.
    let mut files: Vec<(String, String)> = (0..18)
        .map(|level| {
            (
                format!("k{level:02}/kustomization.yaml"),
                format!("resources:\n  - ../k{:02}\n", level + 1),
            )
        })
        .collect();
    files.push((
        "k18/kustomization.yaml".into(),
        "resources:\n  - deployment.yaml\n".into(),
    ));
    files.push(("k18/deployment.yaml".into(), deployment()));
    let files: Vec<(&str, String)> = files
        .iter()
        .map(|(path, body)| (path.as_str(), body.clone()))
        .collect();
    let out = extract_owned(&files);
    let deep = "workload:local/app@k18/deployment.yaml#0:Deployment/web";
    let declared = |level: usize| {
        out.edges.iter().find(|edge| {
            edge.src == format!("env:local/app@k{level:02}/kustomization.yaml")
                && edge.label == "DECLARES"
                && edge.dst == deep
        })
    };
    // 16 kustomizations lie between k02 and the file; 17 between k01 and it.
    let reached = declared(2).expect("k02 reaches the workload");
    assert_eq!(reached.props["via"].as_array().unwrap().len(), MAX_DEPTH);
    assert!(declared(1).is_none());
    assert!(declared(0).is_none());
    assert_eq!(
        reasons(kustomization_gap(&out, "k01/kustomization.yaml")),
        [
            "the `resources` chain is deeper than 16 kustomizations, so \
             `k18/kustomization.yaml` is not followed"
        ]
    );
    assert!(
        out.nodes
            .iter()
            .all(|node| node.id != "gap:environment:local/app@k02/kustomization.yaml")
    );
}

const MAX_DEPTH: usize = 16;

// AC-0662: a kustomization that does not parse, or is not a mapping, is one
// Gap and no Environment; one without `resources` is an Environment citing
// its first key; an entry in another kustomization that is not followed is
// stated on the environment that reaches through it.
#[test]
fn an_unreadable_kustomization_is_a_gap_and_one_without_resources_cites_its_first_key() {
    let bare = "namePrefix: dev-\nnamespace: tools\n";
    let out = extract_owned(&[
        (
            "broken/kustomization.yaml",
            "resources: [unclosed\n".to_string(),
        ),
        ("list/kustomization.yaml", "- deployment.yaml\n".to_string()),
        ("bare/kustomization.yaml", bare.to_string()),
        (
            "over/kustomization.yaml",
            "resources:\n  - ../broken\n  - ../under\n".to_string(),
        ),
        (
            "under/kustomization.yaml",
            "resources:\n  - https://example.com/remote.yaml\n".to_string(),
        ),
    ]);
    assert_eq!(out.files, 5);
    assert_eq!(
        labelled(&out, "Environment"),
        [
            "env:local/app@bare/kustomization.yaml",
            "env:local/app@over/kustomization.yaml",
            "env:local/app@under/kustomization.yaml",
        ]
    );
    assert_eq!(
        kustomization_gap(&out, "broken/kustomization.yaml").props["reason"],
        "the file is not valid YAML"
    );
    assert_eq!(
        kustomization_gap(&out, "list/kustomization.yaml").props["reason"],
        "the file is not a mapping"
    );
    let environment = node(&out, "env:local/app@bare/kustomization.yaml");
    assert_eq!(cited(&environment.props, bare), ["namePrefix"]);
    assert_eq!(environment.props["name"], "tools");
    assert_eq!(
        reasons(kustomization_gap(&out, "over/kustomization.yaml")),
        [
            "`resources` entry `../broken` names a kustomization file that is not read",
            "`under/kustomization.yaml` has entries that are not followed, so what is reached \
             through it is incomplete",
        ]
    );
    assert_eq!(
        node(&out, "env:local/app@over/kustomization.yaml").props["includes"],
        serde_json::json!(["under/kustomization.yaml"])
    );
}

// AC-0662: a document of a read kind without a plain `metadata.name` is a
// Gap and no Workload; one whose `containers` is not a sequence of mappings,
// or that uses an anchor, alias or tag, is a Gap and its literal properties
// are still recorded.
#[test]
fn documents_that_cannot_be_read_exactly_are_gaps_and_literal_properties_stay() {
    let text = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  labels: {}\nspec:\n  \
                replicas: 2\n---\napiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: \
                listless\nspec:\n  replicas: 4\n  template:\n    spec:\n      containers: \
                none\n---\napiVersion: apps/v1\nkind: StatefulSet\nmetadata:\n  name: bare\n\
                spec:\n  replicas: 1\n---\napiVersion: apps/v1\nkind: Deployment\nmetadata:\n  \
                name: anchored\nspec:\n  template:\n    spec:\n      containers:\n        - \
                &main\n          name: app\n          image: !!str app:1\n        - *main\n";
    let out = extract(&[("k8s/odd.yaml", text)]);
    assert_eq!(out.files, 1);
    assert_eq!(
        labelled(&out, "Workload"),
        [
            "workload:local/app@k8s/odd.yaml#1:Deployment/listless",
            "workload:local/app@k8s/odd.yaml#2:StatefulSet/bare",
            "workload:local/app@k8s/odd.yaml#3:Deployment/anchored",
        ]
    );
    let gap = |index: usize| {
        let gap = node(
            &out,
            &format!("gap:environment:local/app@k8s/odd.yaml#{index}"),
        );
        assert_eq!(gap.label, "Gap");
        assert_eq!(gap.props["kind"], "manifest-document");
        assert_eq!(gap.props["path"], "k8s/odd.yaml");
        assert_eq!(gap.props["document"], index);
        assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");
        gap
    };
    let nameless = gap(0);
    assert_eq!(
        nameless.props["reason"],
        "the document has no plain scalar `metadata.name`, so no workload is recorded"
    );
    assert!(nameless.props.get("workload").is_none());
    assert!(cited(&nameless.props, text)[0].starts_with("apiVersion: apps/v1"));

    let listless = gap(1);
    assert_eq!(listless.props["workload"], "Deployment/listless");
    assert_eq!(
        listless.props["reason"],
        "`containers` is not a sequence of mappings"
    );
    assert_eq!(cited(&listless.props, text), ["containers: none"]);
    let workload = node(
        &out,
        "workload:local/app@k8s/odd.yaml#1:Deployment/listless",
    );
    assert_eq!(workload.props["replicas"], 4);
    assert!(workload.props.get("containers").is_none());

    assert_eq!(
        gap(2).props["reason"],
        "`spec.template.spec.containers` is not a sequence of mappings"
    );

    let anchored = gap(3);
    assert_eq!(
        reasons(anchored),
        [
            "a YAML anchor is not evaluated",
            "a YAML alias is not evaluated",
            "a YAML tag is not evaluated",
            "`image` is not written in a form this reader reads",
            "`containers` is not a sequence of mappings",
        ]
    );
    let workload = node(
        &out,
        "workload:local/app@k8s/odd.yaml#3:Deployment/anchored",
    );
    assert_eq!(workload.props["name"], "anchored");
    assert_eq!(gaps(&out).len(), 4);
}

// AC-0662: a manifest workload that no kustomization reaches is still a
// Workload and has no `DECLARES` relation; a file that does not parse (a
// Helm template) yields nothing.
#[test]
fn a_workload_no_kustomization_reaches_has_no_declares() {
    let out = extract_owned(&[
        ("k8s/deployment.yaml", deployment()),
        (
            "chart/templates/deployment.yaml",
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: {{ .Release.Name }}\n\
             spec:\n  replicas: {{ .Values.replicas }}\n{{- if .Values.x }}\n  paused: true\n\
             {{- end }}\n"
                .to_string(),
        ),
        (
            "other/kustomization.yaml",
            "resources:\n  - service.yaml\n".to_string(),
        ),
        (
            "other/service.yaml",
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: web\n".to_string(),
        ),
    ]);
    assert_eq!(
        labelled(&out, "Workload"),
        ["workload:local/app@k8s/deployment.yaml#0:Deployment/web"]
    );
    assert_eq!(
        labelled(&out, "Environment"),
        ["env:local/app@other/kustomization.yaml"]
    );
    // A file of unread kinds is no Gap: coverage records its kinds.
    assert!(out.edges.is_empty());
    assert!(gaps(&out).is_empty(), "{:?}", gaps(&out));
    assert_eq!(out.files, 2);
}

// AC-0662: a `CronJob` stays with the schedule reader. Its facts are the
// same bytes with and without a `Job` manifest in the tree, this reader
// makes a Workload of the `Job` alone, and no id is shared.
#[test]
fn a_cronjob_stays_with_the_schedule_reader_and_a_job_is_not_claimed_twice() {
    let cronjob = "apiVersion: batch/v1\nkind: CronJob\nmetadata:\n  name: nightly\nspec:\n  \
                   schedule: \"0 3 * * *\"\n  jobTemplate:\n    spec:\n      template:\n        \
                   spec:\n          containers:\n            - name: report\n              image: \
                   shop/report:1\n              command: [\"python\", \"report.py\"]\n";
    let job = "apiVersion: batch/v1\nkind: Job\nmetadata:\n  name: seed\nspec:\n  template:\n    \
               spec:\n      containers:\n        - name: seed\n          image: shop/seed:1\n";
    let schedules = |files: &[(&str, &str)]| {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), files);
        let facts = crate::schedules::extract_dir(
            dir.path(),
            &crate::schedules::SourceId {
                repo: REPO,
                commit: "abc",
            },
            &BTreeSet::new(),
        )
        .unwrap();
        (
            serde_json::to_string(&(&facts.nodes, &facts.edges)).unwrap(),
            facts
                .nodes
                .iter()
                .map(|node| node.id.clone())
                .collect::<BTreeSet<String>>(),
        )
    };
    let (alone, scheduled) = schedules(&[("k8s/cron.yaml", cronjob)]);
    let (beside, _) = schedules(&[("k8s/cron.yaml", cronjob), ("k8s/job.yaml", job)]);
    assert!(alone.contains("nightly"));
    assert!(alone.contains("sched:local/app@k8s/cron.yaml#0"));
    assert_eq!(alone, beside);

    // In one file: the schedule reader takes the CronJob document and this
    // reader the Job document.
    let both = format!("{cronjob}---\n{job}");
    let (mixed, mixed_ids) = schedules(&[("k8s/cron.yaml", &both)]);
    assert_eq!(mixed_ids, scheduled);
    assert!(!mixed.contains("seed"));
    let out = extract(&[("k8s/cron.yaml", &both)]);
    assert_eq!(ids(&out), ["workload:local/app@k8s/cron.yaml#1:Job/seed"]);
    assert!(!serialized(&out).contains("nightly"));
    assert!(out.nodes.iter().all(|node| !scheduled.contains(&node.id)));
    // The CronJob alone gives this reader nothing, and coverage nothing.
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &[("k8s/cron.yaml", cronjob)]);
    assert!(extract_in(dir.path()).nodes.is_empty());
    assert!(
        unread(
            "cron.yaml",
            "k8s/cron.yaml",
            &dir.path().join("k8s/cron.yaml")
        )
        .is_empty()
    );
}

// AC-0662: the same tree gives the same facts on every run, whatever order
// its files were written in, and an edited patch is a new content hash.
#[test]
fn kustomize_extraction_is_deterministic_and_independent_of_input_order() {
    let mut files = overlays();
    files.push((
        "overlays/prod/extra.yaml",
        "apiVersion: batch/v1\nkind: Job\nmetadata:\n  name: seed\n".to_string(),
    ));
    files[4].1 = format!("{PROD}namePrefix: prod-\nresources_extra: x\n");
    let first = tempfile::tempdir().unwrap();
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, body)| (*path, body.as_str()))
        .collect();
    write(first.path(), &borrowed);
    let reversed: Vec<(&str, &str)> = borrowed.iter().rev().copied().collect();
    let second = tempfile::tempdir().unwrap();
    write(second.path(), &reversed);

    let one = extract_in(first.path());
    assert_eq!(serialized(&one), serialized(&extract_in(first.path())));
    assert_eq!(serialized(&one), serialized(&extract_in(second.path())));
    // Edges are in a fixed order: by environment, DECLARES before PATCHES.
    let order: Vec<(&str, &str)> = one
        .edges
        .iter()
        .map(|edge| (edge.src.as_str(), edge.label.as_str()))
        .collect();
    assert_eq!(
        order,
        [
            ("env:local/app@base/kustomization.yaml", "DECLARES"),
            ("env:local/app@overlays/prod/kustomization.yaml", "DECLARES"),
            ("env:local/app@overlays/prod/kustomization.yaml", "PATCHES"),
            (
                "env:local/app@overlays/staging/kustomization.yaml",
                "DECLARES"
            ),
            (
                "env:local/app@overlays/staging/kustomization.yaml",
                "PATCHES"
            ),
        ]
    );

    files[3].1 = staging_patch().replace("replicas: 2", "replicas: 3");
    let edited = extract_owned(&files);
    let hash = |out: &EnvironmentExtraction| {
        edge(
            out,
            "env:local/app@overlays/staging/kustomization.yaml",
            "PATCHES",
            WEB,
        )
        .props["prov"]["content_hash"]
            .clone()
    };
    assert_ne!(hash(&one), hash(&edited));
}

// AC-0662: kustomization files and manifest files that yield a fact count
// as files of the producer in the coverage inventory.
#[test]
fn kustomizations_and_manifests_count_in_the_coverage_inventory() {
    let out = extract_owned(&overlays());
    let count = crate::coverage::ProducerCount {
        files: out.files,
        nodes: out.nodes.len() as u64,
        edges: out.edges.len() as u64,
    };
    // Three kustomizations and the base manifest; the patch files are not
    // counted.
    assert_eq!((count.files, count.nodes, count.edges), (4, 4, 5));
}
