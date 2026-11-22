use super::*;

const REPO: &str = "local/app";

/// A value that must never be stored.
const SECRET: &str = "hunter2-sentinel-value";
/// A token-shaped run: 40 or more token characters.
const TOKEN: &str = "ghp0123456789abcdefghijklmnopqrstuvwxyzABCDEF";

fn main_compose() -> String {
    format!(
        r#"name: shop
services:
  web:
    image: nginx:1.27
    build: ./web
    ports:
      - "8080:80"
      - target: 443
        published: 8443
        protocol: tcp
    environment:
      - SESSION_KEY={SECRET}
      - LOG_LEVEL
      - API_URL=http://api:3000
    env_file: web.env
    depends_on:
      - api
      - db
    profiles: [frontend, debug]
  api:
    image: registry.example.com/api:${{TAG}}
    build:
      context: ./api
      dockerfile: Dockerfile.prod
    ports: [3000]
    environment:
      DATABASE_PASSWORD: {SECRET}
      DATABASE_URL: postgres://shop:{SECRET}@db/shop
    env_file:
      - api.env
      - path: shared.env
        required: false
    depends_on:
      db:
        condition: service_healthy
  db:
    image: postgres:16
"#
    )
}

fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, body) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
}

fn extract_in(root: &Path) -> EnvironmentExtraction {
    extract_dir(
        root,
        &SourceId {
            repo: REPO,
            commit: "abc",
        },
    )
    .unwrap()
}

fn extract(files: &[(&str, &str)]) -> EnvironmentExtraction {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), files);
    extract_in(dir.path())
}

fn node<'a>(out: &'a EnvironmentExtraction, id: &str) -> &'a Node {
    out.nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| panic!("no node {id} among {:?}", ids(out)))
}

fn ids(out: &EnvironmentExtraction) -> Vec<&str> {
    out.nodes.iter().map(|node| node.id.as_str()).collect()
}

fn edge<'a>(out: &'a EnvironmentExtraction, src: &str, label: &str, dst: &str) -> &'a Edge {
    out.edges
        .iter()
        .find(|edge| edge.src == src && edge.label == label && edge.dst == dst)
        .unwrap_or_else(|| panic!("no edge {src} {label} {dst}"))
}

/// The source text each evidence span of a fact cites.
fn cited<'a>(props: &serde_json::Value, text: &'a str) -> Vec<&'a str> {
    props["prov"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|span| {
            &text[span["byte_start"].as_u64().unwrap() as usize
                ..span["byte_end"].as_u64().unwrap() as usize]
        })
        .collect()
}

fn gaps(out: &EnvironmentExtraction) -> Vec<&Node> {
    out.nodes
        .iter()
        .filter(|node| node.label == "Gap")
        .collect()
}

fn serialized(out: &EnvironmentExtraction) -> String {
    serde_json::to_string(&(&out.nodes, &out.edges)).unwrap()
}

// AC-0661: a Compose file with a `services` mapping is one Environment
// identified by its path and named only by its top-level `name`, with one
// Workload per service carrying every property SPEC-14 lists, each fact T0
// Confirmed and citing the span that declares it.
#[test]
fn a_compose_file_is_an_environment_with_a_workload_per_service() {
    let text = main_compose();
    let out = extract(&[("compose.yaml", &text)]);
    assert_eq!(out.files, 1);
    assert!(gaps(&out).is_empty(), "{:?}", gaps(&out));
    for props in out
        .nodes
        .iter()
        .map(|node| &node.props)
        .chain(out.edges.iter().map(|edge| &edge.props))
    {
        assert_eq!(props["prov"]["tier"], "Deterministic");
        assert_eq!(props["prov"]["confidence_tier"], "Confirmed");
        assert_eq!(props["prov"]["extractor_id"], ENVIRONMENT_EXTRACTOR_ID);
        assert_eq!(props["prov"]["evidence"][0]["path"], "compose.yaml");
        assert_eq!(props["prov"]["evidence"][0]["commit_sha"], "abc");
    }

    let environment = node(&out, "env:local/app@compose.yaml");
    assert_eq!(environment.label, "Environment");
    assert_eq!(environment.props["declared_by"], "compose");
    assert_eq!(environment.props["path"], "compose.yaml");
    assert_eq!(environment.props["name"], "shop");
    assert_eq!(cited(&environment.props, &text), ["services"]);
    let name_span = &environment.props["name_span"];
    assert_eq!(
        &text[name_span[0].as_u64().unwrap() as usize..name_span[1].as_u64().unwrap() as usize],
        "name: shop"
    );

    let web = node(&out, "workload:local/app@compose.yaml#web");
    assert_eq!(web.label, "Workload");
    assert_eq!(web.props["kind"], "compose-service");
    assert_eq!(web.props["name"], "web");
    assert_eq!(web.props["path"], "compose.yaml");
    assert_eq!(web.props["image"], "nginx:1.27");
    assert_eq!(web.props["build"], "./web");
    // Short form as written; long form as its known keys, in file order.
    assert_eq!(
        web.props["ports"],
        serde_json::json!(["8080:80", "target=443 published=8443 protocol=tcp"])
    );
    // List form: names only, sorted.
    assert_eq!(
        web.props["environment_keys"],
        serde_json::json!(["API_URL", "LOG_LEVEL", "SESSION_KEY"])
    );
    assert_eq!(web.props["env_files"], serde_json::json!(["web.env"]));
    assert_eq!(web.props["depends_on"], serde_json::json!(["api", "db"]));
    assert_eq!(
        web.props["profiles"],
        serde_json::json!(["frontend", "debug"])
    );
    assert!(web.props.get("interpolated").is_none());
    let span = cited(&web.props, &text);
    assert!(span[0].starts_with("image: nginx:1.27"));
    assert!(span[0].trim_end().ends_with("profiles: [frontend, debug]"));

    let api = node(&out, "workload:local/app@compose.yaml#api");
    // Map form of `build`: the context path only.
    assert_eq!(api.props["build"], "./api");
    assert_eq!(api.props["ports"], serde_json::json!(["3000"]));
    // Map form: names only, sorted.
    assert_eq!(
        api.props["environment_keys"],
        serde_json::json!(["DATABASE_PASSWORD", "DATABASE_URL"])
    );
    assert_eq!(
        api.props["env_files"],
        serde_json::json!(["api.env", "shared.env"])
    );
    assert_eq!(api.props["depends_on"], serde_json::json!(["db"]));

    let db = node(&out, "workload:local/app@compose.yaml#db");
    assert_eq!(db.props["image"], "postgres:16");
    for absent in [
        "build",
        "ports",
        "environment_keys",
        "env_files",
        "depends_on",
        "profiles",
    ] {
        assert!(db.props.get(absent).is_none(), "{absent}");
    }

    // DECLARES cites the service key; DEPENDS_ON the entry that names it.
    let declares = edge(
        &out,
        "env:local/app@compose.yaml",
        "DECLARES",
        "workload:local/app@compose.yaml#api",
    );
    assert_eq!(cited(&declares.props, &text), ["api"]);
    let listed = edge(
        &out,
        "workload:local/app@compose.yaml#web",
        "DEPENDS_ON",
        "workload:local/app@compose.yaml#db",
    );
    assert_eq!(cited(&listed.props, &text), ["db"]);
    let mapped = edge(
        &out,
        "workload:local/app@compose.yaml#api",
        "DEPENDS_ON",
        "workload:local/app@compose.yaml#db",
    );
    assert!(cited(&mapped.props, &text)[0].starts_with("db:\n        condition: service_healthy"));
    assert_eq!(out.nodes.len(), 4);
    assert_eq!(out.edges.len(), 3 + 3);
}

// AC-0661 (ADR-0070 decision 5): no value of an environment key reaches a
// fact, in either form of `environment`, and a token-shaped image or port is
// withheld with the count disclosed.
#[test]
fn environment_values_are_never_stored_and_token_shaped_scalars_are_withheld() {
    let text = main_compose();
    let out = extract(&[("compose.yaml", &text)]);
    let stored = serialized(&out);
    assert!(text.contains(SECRET));
    assert!(!stored.contains(SECRET));
    assert!(!stored.contains("postgres://"));
    assert!(!stored.contains("http://api:3000"));
    assert!(!stored.contains("service_healthy"));
    assert!(!stored.contains("Dockerfile.prod"));

    let leaky = format!(
        "services:\n  web:\n    image: registry.example.com/{TOKEN}:1\n    ports:\n      - \
         \"{TOKEN}:80\"\n    command: run --password {SECRET}\n    labels:\n      owner: {SECRET}\n"
    );
    let out = extract(&[("compose.yaml", &leaky)]);
    let stored = serialized(&out);
    assert!(!stored.contains(TOKEN));
    assert!(!stored.contains(SECRET));
    let web = node(&out, "workload:local/app@compose.yaml#web");
    assert_eq!(web.props["image"], "[WITHHELD]:1");
    assert_eq!(web.props["ports"], serde_json::json!(["[WITHHELD]:80"]));
    assert_eq!(web.props["redactions"], 2);
}

// AC-0661: a scalar containing `${` is stored as written and its property
// is marked; it is not resolved.
#[test]
fn an_interpolated_scalar_is_stored_as_written_and_marked() {
    let text = main_compose();
    let out = extract(&[("compose.yaml", &text)]);
    let api = node(&out, "workload:local/app@compose.yaml#api");
    assert_eq!(api.props["image"], "registry.example.com/api:${TAG}");
    assert_eq!(
        api.props["interpolated"],
        serde_json::json!({"image": true})
    );

    let out = extract(&[(
        "compose.yaml",
        "name: ${PROJECT}\nservices:\n  web:\n    image: nginx\n    ports:\n      - \
         \"${PORT}:80\"\n      - \"9000:9000\"\n",
    )]);
    let environment = node(&out, "env:local/app@compose.yaml");
    assert_eq!(environment.props["name"], "${PROJECT}");
    assert_eq!(
        environment.props["interpolated"],
        serde_json::json!({"name": true})
    );
    let web = node(&out, "workload:local/app@compose.yaml#web");
    assert_eq!(
        web.props["ports"],
        serde_json::json!(["${PORT}:80", "9000:9000"])
    );
    assert_eq!(
        web.props["interpolated"],
        serde_json::json!({"ports": true})
    );
}

// AC-0661: a file that does not parse, or has no `services` mapping, is one
// file Gap and no Environment.
#[test]
fn an_unparseable_file_and_a_file_without_services_are_gaps() {
    let broken = "services:\n  web:\n    image: [unclosed\n";
    let out = extract(&[
        ("compose.yaml", broken),
        ("deploy/docker-compose.yml", "volumes:\n  data: {}\n"),
        ("list/compose.yml", "services:\n  - web\n"),
    ]);
    assert_eq!(out.files, 3);
    assert!(out.edges.is_empty());
    assert!(out.nodes.iter().all(|node| node.label == "Gap"));
    let gap = node(&out, "gap:environment:local/app@compose.yaml");
    assert_eq!(gap.props["reason"], "the file is not valid YAML");
    assert_eq!(gap.props["kind"], "compose-file");
    assert_eq!(gap.props["path"], "compose.yaml");
    assert_eq!(gap.props["attempted_tiers"], serde_json::json!(["T0"]));
    assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");
    assert_eq!(gap.props["prov"]["tier"], "Deterministic");
    assert_eq!(cited(&gap.props, broken), [broken]);
    assert_eq!(
        node(&out, "gap:environment:local/app@deploy/docker-compose.yml").props["reason"],
        "the file has no `services` mapping"
    );
    assert_eq!(
        node(&out, "gap:environment:local/app@list/compose.yml").props["reason"],
        "the file has no `services` mapping"
    );
}

// AC-0661: `extends`, an anchor, an alias, a tag and a top-level `include`
// are stated by a Gap, and the properties written literally are still
// recorded.
#[test]
fn constructs_that_are_not_evaluated_are_gaps_and_literal_properties_stay() {
    let text = "include:\n  - other.yaml\n\
x-common: &common\n  restart: always\n\
services:\n\
\x20 base:\n    image: app:1\n    extends:\n      service: parent\n\
\x20 worker:\n    <<: *common\n    image: worker:2\n\
\x20 tagged:\n    image: !!str tagged:3\n    ports:\n      - \"1:1\"\n\
\x20 anchored: &anchored\n    image: anchored:4\n";
    let out = extract(&[("compose.yaml", text)]);

    // The Environment and every Workload are still recorded.
    assert_eq!(
        node(&out, "env:local/app@compose.yaml").label,
        "Environment"
    );
    let workload = |name: &str| node(&out, &format!("workload:local/app@compose.yaml#{name}"));
    assert_eq!(workload("base").props["image"], "app:1");
    assert_eq!(workload("worker").props["image"], "worker:2");
    assert_eq!(
        workload("tagged").props["ports"],
        serde_json::json!(["1:1"])
    );
    assert_eq!(workload("anchored").props["image"], "anchored:4");

    let file = node(&out, "gap:environment:local/app@compose.yaml");
    assert_eq!(file.props["reason"], "top-level `include` is not evaluated");
    assert_eq!(file.props["kind"], "compose-file");
    assert_eq!(cited(&file.props, text), ["include:\n  - other.yaml"]);

    let gap = |name: &str| {
        node(
            &out,
            &format!("gap:environment:local/app@compose.yaml#{name}"),
        )
    };
    let base = gap("base");
    assert_eq!(base.props["reason"], "`extends` is not evaluated");
    assert_eq!(base.props["kind"], "compose-service");
    assert_eq!(base.props["service"], "base");
    assert_eq!(
        cited(&base.props, text),
        ["extends:\n      service: parent"]
    );
    let worker = gap("worker");
    assert_eq!(worker.props["reason"], "a YAML alias is not evaluated");
    assert_eq!(cited(&worker.props, text), ["*common"]);
    let tagged = gap("tagged");
    // The tagged image is not a literal scalar, so it is not stored.
    assert!(workload("tagged").props.get("image").is_none());
    assert_eq!(
        tagged.props["reason"],
        "a YAML tag is not evaluated; `image` is not written in a form this reader reads"
    );
    let anchored = gap("anchored");
    assert_eq!(anchored.props["reason"], "a YAML anchor is not evaluated");
    assert_eq!(cited(&anchored.props, text), ["&anchored"]);
    assert_eq!(gaps(&out).len(), 5);
}

// AC-0661 (R-INT-4): a `depends_on` entry naming a service the file does not
// declare is a Gap, not an edge.
#[test]
fn a_dependency_on_an_undeclared_service_is_a_gap_not_an_edge() {
    let text = "services:\n  web:\n    image: nginx\n    depends_on:\n      - cache\n      - db\n  db:\n    image: postgres\n";
    let out = extract(&[("compose.yaml", text)]);
    let depends: Vec<&Edge> = out
        .edges
        .iter()
        .filter(|edge| edge.label == "DEPENDS_ON")
        .collect();
    assert_eq!(depends.len(), 1);
    assert_eq!(depends[0].dst, "workload:local/app@compose.yaml#db");
    assert!(out.edges.iter().all(|edge| !edge.dst.starts_with("gap:")));
    let gap = node(&out, "gap:environment:local/app@compose.yaml#web");
    assert_eq!(
        gap.props["reason"],
        "`depends_on` names `cache`, a service this file does not declare"
    );
    assert_eq!(cited(&gap.props, text), ["cache"]);
    // The property still lists what the file writes.
    assert_eq!(
        node(&out, "workload:local/app@compose.yaml#web").props["depends_on"],
        serde_json::json!(["cache", "db"])
    );
}

// AC-0661 (ADR-0070 decision 3): an environment file whose environment is
// carried only by its name is a Gap and no Environment; a template name is
// neither. The Gap has no span and no value, and states no name.
#[test]
fn a_name_only_environment_file_is_a_gap_and_a_template_is_not() {
    let body = format!("DATABASE_URL={SECRET}\n");
    let out = extract(&[
        (".env", &body),
        (".env.example", &body),
        (".env.production", &body),
        ("config/staging.env", &body),
        ("env.sample", &body),
        ("env.template", &body),
        ("example.env", &body),
        ("production.yaml", "debug: false\n"),
    ]);
    assert_eq!(
        ids(&out),
        [
            "gap:environment:local/app@.env.production",
            "gap:environment:local/app@config/staging.env",
        ]
    );
    assert_eq!(out.files, 2);
    assert!(out.edges.is_empty());
    for gap in &out.nodes {
        assert_eq!(gap.label, "Gap");
        assert_eq!(gap.props["reason"], NAME_ONLY_REASON);
        assert_eq!(gap.props["kind"], "name-only-environment");
        assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");
        // No readable window into a file that holds secrets (AC-0097).
        assert_eq!(gap.props["prov"]["evidence"], serde_json::json!([]));
        assert!(gap.props.get("name").is_none());
    }
    let stored = serialized(&out);
    assert!(!stored.contains(SECRET));
    assert!(!stored.contains("DATABASE_URL"));
    // The toolchain reader recognises exactly the same files.
    for name in [".env.production", "staging.env", ".env", "example.env"] {
        assert!(crate::toolchain::is_env_name(name));
    }
    assert!(!names_an_environment("environment.yml"));
    assert!(names_an_environment(".env.production.example"));
    assert!(!names_an_environment(".env.EXAMPLE"));
}

// AC-0661 (ADR-0070 decisions 2 and 6): two Compose files are two
// Environments, neither named from its path, and nothing is merged.
#[test]
fn two_compose_files_are_two_environments_and_neither_is_named_from_its_path() {
    let base = "services:\n  web:\n    image: shop:1\n";
    let prod = "services:\n  web:\n    image: shop:1\n    ports:\n      - \"443:8443\"\n";
    let out = extract(&[
        ("docker-compose.yml", base),
        ("docker-compose.prod.yml", prod),
    ]);
    assert_eq!(out.files, 2);
    let environments: Vec<&Node> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Environment")
        .collect();
    assert_eq!(environments.len(), 2);
    for environment in environments {
        assert!(environment.props.get("name").is_none());
        assert!(environment.props.get("name_span").is_none());
        let props = environment.props.as_object().unwrap();
        let keys: Vec<&str> = props.keys().map(String::as_str).collect();
        assert_eq!(keys, ["declared_by", "path", "prov"]);
    }
    assert!(!serialized(&out).contains("\"prod\""));
    let plain = node(&out, "workload:local/app@docker-compose.yml#web");
    let prod = node(&out, "workload:local/app@docker-compose.prod.yml#web");
    assert!(plain.props.get("ports").is_none());
    assert_eq!(prod.props["ports"], serde_json::json!(["443:8443"]));
    assert!(gaps(&out).is_empty());
}

// SPEC-14: the names read are `compose` and `docker-compose` with at most
// one infix.
#[test]
fn compose_file_names() {
    for name in [
        "compose.yaml",
        "compose.yml",
        "docker-compose.yaml",
        "docker-compose.yml",
        "compose.prod.yaml",
        "docker-compose.override.yml",
    ] {
        assert!(compose_file(name), "{name}");
    }
    for name in [
        "compose.json",
        "compose.a.b.yml",
        "docker-compose-dev.yml",
        "composer.yml",
        "my-compose.yml",
        "compose..yml",
    ] {
        assert!(!compose_file(name), "{name}");
    }
}

// AC-0661: the same tree gives the same facts on every run, whatever order
// its files were written in.
#[test]
fn extraction_is_deterministic_and_independent_of_input_order() {
    let main = main_compose();
    let files: Vec<(&str, &str)> = vec![
        ("compose.yaml", &main),
        (
            "deploy/compose.prod.yaml",
            "services:\n  web:\n    image: x\n    extends: base\n",
        ),
        ("staging.env", "A=1\n"),
        ("broken/compose.yml", "services: [\n"),
    ];
    let first = tempfile::tempdir().unwrap();
    write(first.path(), &files);
    let reversed: Vec<(&str, &str)> = files.iter().rev().copied().collect();
    let second = tempfile::tempdir().unwrap();
    write(second.path(), &reversed);

    let one = extract_in(first.path());
    assert_eq!(serialized(&one), serialized(&extract_in(first.path())));
    assert_eq!(serialized(&one), serialized(&extract_in(second.path())));
    assert_eq!(one.files, 4);
    // Content hashes cover the properties: an edited image is a new hash.
    let edited = extract(&[("compose.yaml", &main.replace("postgres:16", "postgres:17"))]);
    let hash = |out: &EnvironmentExtraction| {
        node(out, "workload:local/app@compose.yaml#db").props["prov"]["content_hash"].clone()
    };
    assert_ne!(hash(&one), hash(&edited));
}

// AC-0661 (ADR-0070 consequences): the toolchain reader's Compose `Tool`
// fact is what it was before this reader existed.
#[test]
fn the_compose_tool_fact_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[(
            "docker-compose.yml",
            "services:\n  web:\n    image: nginx:1.27\n  db:\n    image: postgres:16\n",
        )],
    );
    let tools = crate::toolchain::extract_dir(
        dir.path(),
        &crate::toolchain::SourceId {
            repo: REPO,
            commit: "abc",
        },
        &mut |_| {},
    )
    .unwrap();
    let tool = tools
        .nodes
        .iter()
        .find(|node| node.id == "tool:local/app@docker-compose.yml")
        .unwrap();
    // The fact as the toolchain reader emitted it before #1791, byte for byte.
    assert_eq!(
        serde_json::to_string(&tool.props).unwrap(),
        "{\"category\":\"container\",\"display\":\"Docker Compose\",\
         \"name\":\"docker-compose.yml\",\"prov\":{\"confidence_tier\":\"Confirmed\",\
         \"content_hash\":\"dae54b9384c94368f1543fca3a2263ee1716ba70d3dcfb082b1dea17856eae51\",\
         \"evidence\":[{\"byte_end\":9,\"byte_start\":0,\"commit_sha\":\"abc\",\
         \"path\":\"docker-compose.yml\",\"repo\":\"local/app\"}],\
         \"extractor_id\":\"t0.toolchain\",\"tier\":\"Deterministic\"},\
         \"settings\":{\"images\":[\"nginx:1.27\",\"postgres:16\"],\
         \"services\":[\"web\",\"db\"]}}"
    );
    // No environment fact shares an id with a toolchain fact.
    let environments = extract_in(dir.path());
    let tool_ids: BTreeSet<&str> = tools.nodes.iter().map(|node| node.id.as_str()).collect();
    assert!(
        environments
            .nodes
            .iter()
            .all(|node| !tool_ids.contains(node.id.as_str()))
    );
}

// AC-0661, AC-0662: Helm charts and values files are recorded in coverage as
// present and not read, and so is each Kubernetes manifest document of a
// kind this reader does not read, by kind, with its count and first site. A
// file that holds only read kinds, a kustomization and a `CronJob` the
// schedule reader covers are not reported, and arbitrary YAML is not
// reported at all. The name is the one `docs/test-map.md` binds T-0661 to;
// kustomize files and the pod-bearing kinds are read since increment 2.
#[test]
fn kubernetes_kustomize_and_helm_files_are_present_and_not_read() {
    let deployment = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n";
    let cronjob = "apiVersion: batch/v1\nkind: CronJob\nmetadata:\n  name: nightly\nspec:\n  \
                   schedule: \"0 3 * * *\"\n";
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            ("k8s/deployment.yaml", deployment),
            (
                "k8s/extra.yaml",
                "apiVersion: networking.k8s.io/v1\nkind: Ingress\n---\napiVersion: v1\nkind: \
                 Service\n",
            ),
            (
                "k8s/service.yml",
                "# a service\napiVersion: v1\nkind: Service\n---\napiVersion: v1\nkind: ConfigMap\n",
            ),
            ("k8s/cron.yaml", cronjob),
            ("k8s/mixed.yaml", &format!("{cronjob}---\n{deployment}")),
            ("k8s/odd.yaml", "apiVersion: v1\nkind: \"not a kind\"\n"),
            (
                "overlays/prod/kustomization.yaml",
                "apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nresources:\n  \
                 - ../../k8s/deployment.yaml\n",
            ),
            (
                "chart/Chart.yaml",
                "apiVersion: v2\nname: shop\nversion: 1.0.0\n",
            ),
            ("chart/values.yaml", "replicas: 2\n"),
            ("chart/values-prod.yaml", "replicas: 5\n"),
            // Not in a chart: arbitrary YAML.
            ("config/values.yaml", "replicas: 2\n"),
            ("config/app.yaml", "kind: thing\nname: x\n"),
            (
                "config/nested.yaml",
                "spec:\n  apiVersion: v1\n  kind: Pod\n",
            ),
            ("compose.yaml", "services:\n  web:\n    image: nginx\n"),
        ],
    );
    let report = crate::preflight::preflight(dir.path()).unwrap();
    let coverage = crate::coverage::RepoCoverage::from_facts(&crate::coverage::CoverageFacts {
        repo: REPO,
        commit: "abc",
        ..Default::default()
    })
    .with_unsupported(&report);
    let listed: Vec<_> = coverage
        .unsupported
        .iter()
        .filter(|capability| is_limit_kind(&capability.capability))
        .map(|capability| {
            (
                capability.capability.as_str(),
                capability.occurrences,
                capability.first_site.as_str(),
                capability.reason.as_str(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            (
                "kubernetes-manifest:Ingress",
                1,
                "k8s/extra.yaml:1",
                "Kubernetes `Ingress` manifest is present and not read: no installed producer \
                 recovers environments from it: a tool limitation, not a system gap"
            ),
            (
                "kubernetes-manifest:Service",
                2,
                "k8s/extra.yaml:4",
                "Kubernetes `Service` manifest is present and not read: no installed producer \
                 recovers environments from it: a tool limitation, not a system gap"
            ),
            (
                "kubernetes-manifest:other",
                1,
                "k8s/odd.yaml:1",
                "Kubernetes manifest of a kind not written as a plain name is present and not \
                 read: no installed producer recovers environments from it: a tool limitation, \
                 not a system gap"
            ),
            (
                "kubernetes-manifest:ConfigMap",
                1,
                "k8s/service.yml:5",
                "Kubernetes `ConfigMap` manifest is present and not read: no installed producer \
                 recovers environments from it: a tool limitation, not a system gap"
            ),
        ]
    );
    assert!(coverage.limited);
    // A file of read kinds, the CronJob file and the kustomization are not
    // named here.
    // Helm charts and values files are read since increment 3 (AC-1502).
    for path in [
        "k8s/cron.yaml",
        "k8s/deployment.yaml",
        "k8s/mixed.yaml",
        "overlays/prod/kustomization.yaml",
        "chart/Chart.yaml",
        "chart/values.yaml",
        "chart/values-prod.yaml",
    ] {
        assert!(
            report
                .unsupported
                .iter()
                .filter(|finding| is_limit_kind(&finding.kind))
                .all(|finding| finding.path != path),
            "{path}"
        );
    }
    // A file that is not valid YAML is not a manifest.
    let broken = dir.path().join("broken.yaml");
    std::fs::write(&broken, "apiVersion: v1\nkind: [unclosed\n").unwrap();
    assert!(unread("broken.yaml", "broken.yaml", &broken).is_empty());
}

// AC-0661: the reader is a producer in the coverage inventory, with its
// file, node and edge counts.
#[test]
fn the_reader_is_a_producer_in_the_coverage_inventory() {
    let dir = tempfile::tempdir().unwrap();
    let text = main_compose();
    write(
        dir.path(),
        &[("compose.yaml", &text), ("staging.env", "A=1\n")],
    );
    let out = extract_in(dir.path());
    let count = crate::coverage::ProducerCount {
        files: out.files,
        nodes: out.nodes.len() as u64,
        edges: out.edges.len() as u64,
    };
    assert_eq!((count.files, count.nodes, count.edges), (2, 5, 6));
    let coverage = crate::coverage::RepoCoverage::from_facts(&crate::coverage::CoverageFacts {
        repo: REPO,
        commit: "abc",
        deterministic: vec![(ENVIRONMENT_EXTRACTOR_ID, count)],
        ..Default::default()
    });
    let producer = coverage
        .producers
        .iter()
        .find(|producer| producer.id == ENVIRONMENT_EXTRACTOR_ID)
        .expect("the environments reader is a listed producer");
    assert_eq!(producer.covers, "environments and workloads");
    assert_eq!(producer.state, crate::coverage::ProducerState::Ran);
    assert_eq!((producer.files, producer.nodes, producer.edges), (2, 5, 6));
    // With no Compose or name-only environment file it is listed as not
    // applicable, never left out.
    let none = crate::coverage::RepoCoverage::from_facts(&crate::coverage::CoverageFacts {
        repo: REPO,
        commit: "abc",
        ..Default::default()
    });
    assert!(none.producers.iter().any(|producer| {
        producer.id == ENVIRONMENT_EXTRACTOR_ID
            && producer.state == crate::coverage::ProducerState::NotApplicable
    }));
}

mod manifests;

fn helm_tree() -> Vec<(&'static str, String)> {
    vec![
        (
            "charts/shop/Chart.yaml",
            "apiVersion: v2\nname: shop\nversion: 1.2.0\nappVersion: \"3.4\"\ndependencies:\n  - name: redis\n    version: 17.x\n".into(),
        ),
        (
            "charts/shop/values.yaml",
            format!("replicaCount: 1\nimage:\n  repository: shop\n  tag: latest\nauth:\n  password: {SECRET}\nhosts: [a, b]\n"),
        ),
        (
            "charts/shop/values-prod.yaml",
            format!("replicaCount: 5\nauth:\n  password: {SECRET}\n"),
        ),
        (
            "charts/shop/values-staging.yaml",
            "replicaCount: 2\n".into(),
        ),
        // Named by no helmfile: the name alone names an environment.
        ("charts/shop/values-qa.yaml", "replicaCount: 1\n".into()),
        // A template word names no environment.
        ("charts/shop/values.example.yaml", "replicaCount: 1\n".into()),
        (
            "deploy/helmfile.yaml",
            format!(
                r#"environments:
  production: {{}}
  staging: {{}}
releases:
  - name: shop-prod
    namespace: shop-production
    chart: ../charts/shop
    values:
      - ../charts/shop/values-prod.yaml
    set:
      - name: auth.password
        value: {SECRET}
  - name: shop-staging
    namespace: shop-staging
    chart: ../charts/shop
    values:
      - ../charts/shop/values-staging.yaml
      - ../charts/shop/values-missing.yaml
      - replicaCount: 3
  - name: cache
    chart: bitnami/redis
"#
            ),
        ),
    ]
}

fn extract_helm() -> EnvironmentExtraction {
    let dir = tempfile::tempdir().unwrap();
    let files = helm_tree();
    let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
    write(dir.path(), &files);
    extract_in(dir.path())
}

fn node_of<'a>(out: &'a EnvironmentExtraction, id: &str) -> &'a Node {
    out.nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| {
            panic!(
                "missing {id}: {:?}",
                out.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
            )
        })
}

// AC-1502 (T-1502, #2167): a chart is a workload with its default values'
// key paths; each helmfile release is an environment that declares its
// in-repository chart with the key paths of the values files it names. No
// value is stored, and no values file is cited by span.
#[test]
fn helmfile_releases_declare_their_chart_with_values_keys() {
    let out = extract_helm();
    let chart = node_of(&out, &format!("workload:{REPO}@charts/shop/Chart.yaml"));
    assert_eq!(chart.label, "Workload");
    assert_eq!(chart.props["kind"], "helm-chart");
    assert_eq!(chart.props["name"], "shop");
    assert_eq!(chart.props["version"], "1.2.0");
    assert_eq!(chart.props["app_version"], "3.4");
    assert_eq!(chart.props["dependencies"], serde_json::json!(["redis"]));
    assert_eq!(chart.props["values_file"], "charts/shop/values.yaml");
    assert_eq!(
        chart.props["values_keys"],
        serde_json::json!([
            "auth.password",
            "hosts[]",
            "image.repository",
            "image.tag",
            "replicaCount"
        ])
    );

    let prod = node_of(&out, &format!("env:{REPO}@deploy/helmfile.yaml#shop-prod"));
    assert_eq!(prod.label, "Environment");
    assert_eq!(prod.props["declared_by"], "helmfile");
    assert_eq!(prod.props["path"], "deploy/helmfile.yaml#shop-prod");
    assert_eq!(prod.props["file"], "deploy/helmfile.yaml");
    assert_eq!(prod.props["release"], "shop-prod");
    assert_eq!(prod.props["name"], "shop-production");
    assert_eq!(prod.props["chart"], "../charts/shop");
    assert_eq!(
        prod.props["helmfile_environments"],
        serde_json::json!(["production", "staging"])
    );
    let declares = out
        .edges
        .iter()
        .find(|edge| edge.src == prod.id && edge.label == "DECLARES")
        .unwrap();
    assert_eq!(declares.dst, chart.id);
    assert_eq!(
        declares.props["values"],
        serde_json::json!([{
            "path": "charts/shop/values-prod.yaml",
            "keys": ["auth.password", "replicaCount"]
        }])
    );
    // Every cited span is in the helmfile, never in a values file.
    for fact in out
        .nodes
        .iter()
        .map(|node| &node.props)
        .chain(out.edges.iter().map(|edge| &edge.props))
    {
        for evidence in fact["prov"]["evidence"].as_array().unwrap() {
            let path = evidence["path"].as_str().unwrap();
            assert!(!path.contains("values"), "{path} is cited");
        }
    }
    // No value of any values file or `set` entry is stored.
    let all = serde_json::to_string(&(&out.nodes, &out.edges)).unwrap();
    assert!(!all.contains(SECRET));
    assert!(!all.contains("\"latest\""));
}

// AC-1502: what a release states that is not read, a remote chart and a
// values file named by no release are Gaps; a template word names nothing.
#[test]
fn helm_constructs_not_read_are_gaps() {
    let out = extract_helm();
    let prod_gap = node_of(
        &out,
        &format!("gap:environment:{REPO}@deploy/helmfile.yaml#shop-prod"),
    );
    assert_eq!(prod_gap.props["kind"], "helmfile-release");
    assert!(
        prod_gap.props["reason"]
            .as_str()
            .unwrap()
            .contains("`set` is not read")
    );
    let staging_gap = node_of(
        &out,
        &format!("gap:environment:{REPO}@deploy/helmfile.yaml#shop-staging"),
    );
    let reason = staging_gap.props["reason"].as_str().unwrap();
    assert!(
        reason.contains("values-missing.yaml` is not among the files read"),
        "{reason}"
    );
    assert!(
        reason.contains("inline `values` entry is not read"),
        "{reason}"
    );
    let staging = out
        .edges
        .iter()
        .find(|edge| edge.src == format!("env:{REPO}@deploy/helmfile.yaml#shop-staging"))
        .unwrap();
    assert_eq!(
        staging.props["values"][0]["keys"],
        serde_json::json!(["replicaCount"])
    );
    // A chart outside the repository: an environment, no DECLARES, a Gap.
    let cache = format!("env:{REPO}@deploy/helmfile.yaml#cache");
    node_of(&out, &cache);
    assert!(!out.edges.iter().any(|edge| edge.src == cache));
    let cache_gap = node_of(
        &out,
        &format!("gap:environment:{REPO}@deploy/helmfile.yaml#cache"),
    );
    assert!(
        cache_gap.props["reason"]
            .as_str()
            .unwrap()
            .contains("bitnami/redis")
    );
    // values-qa.yaml is named by no release: a name-only Gap with no span.
    let qa = node_of(
        &out,
        &format!("gap:environment:{REPO}@charts/shop/values-qa.yaml"),
    );
    assert_eq!(qa.props["kind"], "name-only-environment");
    assert!(qa.props["prov"]["evidence"].as_array().unwrap().is_empty());
    // Named values files, the default and a template are not name-only Gaps.
    for path in [
        "values-prod.yaml",
        "values-staging.yaml",
        "values.yaml",
        "values.example.yaml",
    ] {
        assert!(
            !out.nodes
                .iter()
                .any(|node| node.id == format!("gap:environment:{REPO}@charts/shop/{path}")),
            "{path}"
        );
    }
}

// AC-1502: a templated helmfile is a Gap and no environment; output is
// identical across runs.
#[test]
fn templated_helmfiles_are_gaps_and_helm_output_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[(
            "helmfile.yaml",
            "releases:\n  - name: app\n    chart: ./chart\n    values:\n      - values-{{ .Environment.Name }}.yaml\n",
        )],
    );
    let out = extract_in(dir.path());
    assert!(!out.nodes.iter().any(|node| node.label == "Environment"));
    let gap = node_of(&out, &format!("gap:environment:{REPO}@helmfile.yaml"));
    assert_eq!(gap.props["kind"], "helmfile");
    assert!(gap.props["reason"].as_str().unwrap().contains("template"));

    // A withheld environment name is not stored; `values.yml` is not the
    // chart's default values file (Helm reads exactly `values.yaml`).
    let dir = tempfile::tempdir().unwrap();
    let helmfile = format!(
        "environments:\n  {TOKEN}: {{}}\n  prod: {{}}\nreleases:\n  - name: app\n    chart: ./chart\n"
    );
    write(
        dir.path(),
        &[
            ("helmfile.yaml", helmfile.as_str()),
            ("chart/Chart.yaml", "name: app\n"),
            ("chart/values.yml", "replicaCount: 1\n"),
        ],
    );
    let out = extract_in(dir.path());
    let release = node_of(&out, &format!("env:{REPO}@helmfile.yaml#app"));
    assert_eq!(
        release.props["helmfile_environments"],
        serde_json::json!(["prod"])
    );
    let all = serde_json::to_string(&(&out.nodes, &out.edges)).unwrap();
    assert!(!all.contains(TOKEN));
    let chart = node_of(&out, &format!("workload:{REPO}@chart/Chart.yaml"));
    assert!(chart.props.get("values_keys").is_none());

    let a = extract_helm();
    let b = extract_helm();
    assert_eq!(
        serde_json::to_string(&(&a.nodes, &a.edges)).unwrap(),
        serde_json::to_string(&(&b.nodes, &b.edges)).unwrap()
    );
}

const RES: &str = "src/main/resources";

fn spring_tree() -> Vec<(String, String)> {
    vec![
        (
            format!("{RES}/application.yml"),
            format!(
                "spring:\n  application:\n    name: shop\n  profiles:\n    active: prod\n    group:\n      cloud: [prod, metrics]\ndatasource:\n  password: {SECRET}\n---\nspring:\n  config:\n    activate:\n      on-profile: staging\nserver:\n  port: 8081\n---\nspring.config.activate.on-profile: prod & cloud\nfeature: on\n"
            ),
        ),
        (
            format!("{RES}/application-prod.yml"),
            format!("datasource:\n  url: jdbc:postgresql://db/shop\n  password: {SECRET}\n"),
        ),
        (
            format!("{RES}/application-metrics.properties"),
            "management.endpoints.web.exposure.include=*\n".into(),
        ),
        // No base file declares `qa`: named only by the file name.
        (
            format!("{RES}/application-qa.properties"),
            "server.port=9000\n".into(),
        ),
        // A template word names no environment.
        (
            format!("{RES}/application-example.yml"),
            "server:\n  port: 1\n".into(),
        ),
        (
            "worker/application.properties".into(),
            format!(
                "spring.profiles.active=dev\ndb.password={SECRET}\n#---\nspring.config.activate.on-profile=test\nserver.port=1\\\n  2\n"
            ),
        ),
        (
            "worker/application-dev.yaml".into(),
            "cache:\n  ttl: 5\n".into(),
        ),
    ]
}

fn extract_spring() -> EnvironmentExtraction {
    let dir = tempfile::tempdir().unwrap();
    let files = spring_tree();
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    write(dir.path(), &files);
    extract_in(dir.path())
}

// AC-1503 (T-1503, #2167): a Spring Boot document activated for a profile
// is an environment named by it; a profile-specific file is one only where
// a base file of its directory declares the profile. Each lists the key
// paths it sets, never a value, and cites only where the profile is stated.
#[test]
fn spring_profiles_are_environments_only_where_a_file_states_them() {
    let out = extract_spring();
    let staging = node_of(&out, &format!("env:{REPO}@{RES}/application.yml#staging"));
    assert_eq!(staging.label, "Environment");
    assert_eq!(staging.props["declared_by"], "spring-profile");
    assert_eq!(staging.props["name"], "staging");
    assert_eq!(
        staging.props["path"],
        format!("{RES}/application.yml#staging")
    );
    assert_eq!(
        staging.props["settings_keys"],
        serde_json::json!(["server.port"])
    );

    let prod = node_of(&out, &format!("env:{REPO}@{RES}/application-prod.yml"));
    assert_eq!(prod.props["name"], "prod");
    assert_eq!(
        prod.props["profile_declared_in"],
        format!("{RES}/application.yml")
    );
    assert_eq!(
        prod.props["settings_keys"],
        serde_json::json!(["datasource.password", "datasource.url"])
    );
    // A profile named by a group is declared too.
    let metrics = node_of(
        &out,
        &format!("env:{REPO}@{RES}/application-metrics.properties"),
    );
    assert_eq!(
        metrics.props["settings_keys"],
        serde_json::json!(["management.endpoints.web.exposure.include"])
    );
    // `.properties` documents, a continued line and an `active` list.
    let test = node_of(
        &out,
        &format!("env:{REPO}@worker/application.properties#test"),
    );
    assert_eq!(
        test.props["settings_keys"],
        serde_json::json!(["server.port"])
    );
    let dev = node_of(&out, &format!("env:{REPO}@worker/application-dev.yaml"));
    assert_eq!(dev.props["settings_keys"], serde_json::json!(["cache.ttl"]));

    // Only base files are cited, at the statement of the profile.
    for node in out.nodes.iter().filter(|node| node.label == "Environment") {
        for evidence in node.props["prov"]["evidence"].as_array().unwrap() {
            let path = evidence["path"].as_str().unwrap();
            assert!(
                path.ends_with("/application.yml") || path.ends_with("/application.properties"),
                "{path} is cited"
            );
        }
    }
    let all = serde_json::to_string(&(&out.nodes, &out.edges)).unwrap();
    assert!(!all.contains(SECRET));
    assert!(!all.contains("jdbc:"));
}

// AC-1503: an undeclared profile file is a name-only Gap, a template word
// names nothing, and a profile expression is a Gap.
#[test]
fn undeclared_profiles_and_profile_expressions_are_gaps() {
    let out = extract_spring();
    let qa = node_of(
        &out,
        &format!("gap:environment:{REPO}@{RES}/application-qa.properties"),
    );
    assert_eq!(qa.props["kind"], "name-only-environment");
    assert!(qa.props["prov"]["evidence"].as_array().unwrap().is_empty());
    assert!(
        !out.nodes
            .iter()
            .any(|node| node.id.contains("application-example"))
    );
    let gap = node_of(
        &out,
        &format!("gap:environment:{REPO}@{RES}/application.yml"),
    );
    assert_eq!(gap.props["kind"], "spring-config");
    assert!(
        gap.props["reason"]
            .as_str()
            .unwrap()
            .contains("profile expression")
    );
    assert!(
        !out.nodes
            .iter()
            .any(|node| node.id.contains("prod & cloud"))
    );
    let a = extract_spring();
    assert_eq!(
        serde_json::to_string(&(&a.nodes, &a.edges)).unwrap(),
        serde_json::to_string(&(&out.nodes, &out.edges)).unwrap()
    );
}

// AC-1503: a settings module or configuration file named like an
// environment is a name-only Gap; other names and places are nothing.
#[test]
fn settings_files_named_like_environments_are_name_only_gaps() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            ("app/settings/production.py", "DEBUG = False\n"),
            ("app/settings/base.py", "DEBUG = True\n"),
            ("config/staging.yml", "host: example\n"),
            ("config/database.yml", "adapter: postgresql\n"),
            (
                "config/environments/production.rb",
                "Rails.application.configure do\nend\n",
            ),
            ("scripts/production.py", "print(1)\n"),
        ],
    );
    let out = extract_in(dir.path());
    let gaps: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|node| node.props["kind"] == "name-only-environment")
        .map(|node| node.props["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        gaps,
        BTreeSet::from([
            "app/settings/production.py",
            "config/environments/production.rb",
            "config/staging.yml",
        ])
    );
    assert!(!out.nodes.iter().any(|node| node.label == "Environment"));
}

// AC-1503: a Kubernetes manifest named like a Spring Boot file is still read
// as a manifest, and a legacy `spring.profiles` list is a Gap, not a key.
#[test]
fn spring_named_manifests_stay_manifests_and_profile_lists_are_gaps() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            (
                "deploy/application.yml",
                "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n",
            ),
            (
                "deploy/application-prod.yml",
                "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web-prod\n",
            ),
            (
                "app/application.yml",
                "spring:\n  profiles: [prod, cloud]\nserver:\n  port: 1\n",
            ),
        ],
    );
    let out = extract_in(dir.path());
    let workloads: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Workload")
        .filter_map(|node| node.props["name"].as_str())
        .collect();
    assert_eq!(workloads, BTreeSet::from(["web", "web-prod"]));
    // The manifests' own Gaps only: nothing reads them as Spring profiles.
    assert!(
        !out.nodes
            .iter()
            .any(|node| node.id.contains("deploy/application")
                && node.props["kind"] != "manifest-document"
                && node.label == "Gap")
    );
    let gap = node_of(&out, &format!("gap:environment:{REPO}@app/application.yml"));
    assert_eq!(gap.props["kind"], "spring-config");
    assert!(
        gap.props["reason"]
            .as_str()
            .unwrap()
            .contains("profile expression or list")
    );
    assert!(!out.nodes.iter().any(|node| node.label == "Environment"));
}

// AC-1059 (T-1059, #385): over a source capture the reader reads the
// captured bytes of its whole selection and never the checkout: a file
// edited, deleted or created after capture changes nothing, and a selected
// file the capture does not hold is not among the files read.
#[test]
fn captured_bytes_are_read_instead_of_the_checkout() {
    let deployment = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n";
    let mut files: Vec<(String, String)> = helm_tree()
        .into_iter()
        .map(|(path, text)| (path.to_string(), text))
        .collect();
    files.extend([
        ("compose.yaml".to_string(), main_compose()),
        (".env.production".into(), format!("TOKEN={SECRET}\n")),
        ("k8s/deployment.yaml".into(), deployment.into()),
        (
            "overlays/prod/kustomization.yaml".into(),
            "resources:\n  - ../../k8s/deployment.yaml\n".into(),
        ),
        (
            "src/main/resources/application-prod.yml".into(),
            "server:\n  port: 8080\n".into(),
        ),
        ("src/main.rs".into(), "fn main() {}\n".into()),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    write(dir.path(), &borrowed);
    let before = serialized(&extract_in(dir.path()));

    let paths = environment_paths(dir.path()).unwrap();
    assert!(paths.contains(&"compose.yaml".to_string()));
    assert!(paths.contains(&"deploy/helmfile.yaml".to_string()));
    // An env file holds secrets: no capture retains it (AC-0097).
    assert!(!paths.contains(&".env.production".to_string()));
    assert!(!paths.contains(&"src/main.rs".to_string()));
    let captured: std::collections::BTreeMap<String, Vec<u8>> = paths
        .iter()
        .map(|rel| (rel.clone(), std::fs::read(dir.path().join(rel)).unwrap()))
        .collect();

    // After capture: an edit, a deletion and new files.
    write(
        dir.path(),
        &[
            ("compose.yaml", "services:\n  other:\n    image: busybox\n"),
            ("late/compose.yaml", "services:\n  late:\n    image: late\n"),
            (
                "late/kustomization.yaml",
                "resources:\n  - ../k8s/deployment.yaml\n",
            ),
            (
                "k8s/deployment.yaml",
                "apiVersion: v1\nkind: Pod\nmetadata:\n  name: x\n",
            ),
        ],
    );
    std::fs::remove_file(dir.path().join("charts/shop/values-prod.yaml")).unwrap();
    let read = |rel: &str| {
        captured
            .get(rel)
            .cloned()
            .ok_or_else(|| std::io::Error::other("not captured"))
    };
    let keys: Vec<String> = captured.keys().cloned().collect();
    let id = SourceId {
        repo: REPO,
        commit: "abc",
    };
    let after = extract_from_capture(dir.path(), &keys, &read, &id).unwrap();
    assert_eq!(serialized(&after), before);

    // Without the capture the checkout's changes are read.
    assert_ne!(serialized(&extract_in(dir.path())), before);
}

// AC-1059: a values file a helmfile names is retained whatever its name, so
// over a capture its keys never depend on the checkout: deleting it there,
// or creating one the helmfile names after capture, changes nothing.
#[test]
fn helmfile_named_values_files_are_captured() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            (
                "charts/shop/Chart.yaml",
                "apiVersion: v2\nname: shop\nversion: 1.0.0\n",
            ),
            (
                "deploy/helmfile.yaml",
                "releases:\n  - name: shop\n    chart: ../charts/shop\n    values:\n      - prod\n      - late\n",
            ),
            ("deploy/prod", "replicaCount: 5\n"),
        ],
    );
    let before = serialized(&extract_in(dir.path()));
    let paths = environment_paths(dir.path()).unwrap();
    assert!(paths.contains(&"deploy/prod".to_string()), "{paths:?}");
    let captured: std::collections::BTreeMap<String, Vec<u8>> = paths
        .iter()
        .map(|rel| (rel.clone(), std::fs::read(dir.path().join(rel)).unwrap()))
        .collect();
    std::fs::remove_file(dir.path().join("deploy/prod")).unwrap();
    write(dir.path(), &[("deploy/late", "replicaCount: 9\n")]);
    let read = |rel: &str| {
        captured
            .get(rel)
            .cloned()
            .ok_or_else(|| std::io::Error::other("not captured"))
    };
    let keys: Vec<String> = captured.keys().cloned().collect();
    let id = SourceId {
        repo: REPO,
        commit: "abc",
    };
    let after = extract_from_capture(dir.path(), &keys, &read, &id).unwrap();
    assert_eq!(serialized(&after), before);
    assert_ne!(serialized(&extract_in(dir.path())), before);
}
