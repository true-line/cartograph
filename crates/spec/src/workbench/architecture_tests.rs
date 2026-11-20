//! `architecture.md` and `architecture.dsl` (#2049, AC-0940..AC-0942).
use super::*;
use crate::{FactIndex, check_citations, compile_spec};
use core_prov::{EvidenceRef, Provenance, Tier};
use serde_json::{Value, json};

const SERVER: &str = "acme/server";
const CLIENT: &str = "acme/client";
const TOOLS: &str = "acme/tools";

fn prov(tier: Tier, confidence: ConfidenceTier, (repo, path): (&str, &str), fact: &str) -> Value {
    let evidence = vec![EvidenceRef {
        repo: repo.into(),
        path: path.into(),
        byte_start: 1,
        byte_end: 9,
        commit_sha: "0123abc".into(),
    }];
    serde_json::to_value(
        Provenance::new(tier, confidence, evidence, "t0.fixture", fact.as_bytes()).unwrap(),
    )
    .unwrap()
}

#[derive(Default, Clone)]
struct Fixture {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Fixture {
    fn node(&mut self, id: &str, label: &str, mut props: Value, at: (&str, &str)) -> String {
        props["prov"] = prov(Tier::Deterministic, ConfidenceTier::Confirmed, at, id);
        self.nodes.push(Node {
            id: id.into(),
            label: label.into(),
            props,
        });
        id.to_string()
    }
    fn edge(&mut self, src: &str, label: &str, dst: &str, at: (&str, &str), mut props: Value) {
        let confidence = if dst.starts_with("gap:") {
            ConfidenceTier::Gap
        } else {
            ConfidenceTier::Confirmed
        };
        props["prov"] = prov(
            Tier::Deterministic,
            confidence,
            at,
            &format!("{src} {label} {dst}"),
        );
        self.edges.push(Edge {
            src: src.into(),
            dst: dst.into(),
            label: label.into(),
            props,
        });
    }
    /// A file and a symbol defined in it.
    fn code(&mut self, repo: &str, path: &str, name: &str) -> String {
        let file = format!("file:{repo}@{path}");
        if self.nodes.iter().all(|n| n.id != file) {
            self.node(&file, "File", json!({"path": path}), (repo, path));
        }
        let sym = self.node(
            &format!("sym:{repo}@{path}#{name}"),
            "Symbol",
            json!({"name": name}),
            (repo, path),
        );
        self.edge(&sym, "DEFINED_IN", &file, (repo, path), json!({}));
        sym
    }
}

/// A Flask server with an SMTP listener, operator commands, cron jobs, a SQL
/// database and an outside API; a Vue client calling one of its endpoints;
/// and a repository with code and no way in.
fn system() -> Fixture {
    let mut f = Fixture::default();
    for repo in [SERVER, CLIENT, TOOLS] {
        f.node(&format!("repo:{repo}"), "Repo", json!({}), (repo, "."));
    }
    // Web application.
    let ep = f.node(
        &format!("ep:{SERVER}@GET:/api/aliases"),
        "Endpoint",
        json!({"method": "GET", "path": "/api/aliases", "framework": "flask"}),
        (SERVER, "app/api/alias.py"),
    );
    let handler = f.code(SERVER, "app/api/alias.py", "list_aliases");
    f.edge(
        &ep,
        "HANDLES",
        &handler,
        (SERVER, "app/api/alias.py"),
        json!({}),
    );
    let query = f.code(SERVER, "app/models.py", "Alias.query");
    f.edge(
        &handler,
        "CALLS",
        &query,
        (SERVER, "app/api/alias.py"),
        json!({}),
    );
    f.node(
        &format!("entry:{SERVER}@Dockerfile#cmd"),
        "EntryPoint",
        json!({"kind": "http-server", "name": "HTTP server", "file": "wsgi.py"}),
        (SERVER, "Dockerfile"),
    );
    // Data: one SQL store, one mapped table the handler's code writes.
    let db = f.node(
        &format!("res:{SERVER}@observed:sql-database:env:DB_URI"),
        "Resource",
        json!({"kind": "sdk-client", "provider": "sql-database", "module": "sqlalchemy",
               "observed": true, "env_key": "DB_URI",
               "logical_id": "observed:sql-database:env:DB_URI"}),
        (SERVER, "app/db.py"),
    );
    let db_file = format!("file:{SERVER}@app/db.py");
    f.node(
        &db_file,
        "File",
        json!({"path": "app/db.py"}),
        (SERVER, "app/db.py"),
    );
    f.edge(&db_file, "USES", &db, (SERVER, "app/db.py"), json!({}));
    let table = f.node(
        &format!("data:{SERVER}@sqlalchemy:alias"),
        "DataEntity",
        json!({"name": "alias", "framework": "sqlalchemy"}),
        (SERVER, "app/models.py"),
    );
    f.edge(
        &query,
        "WRITES",
        &table,
        (SERVER, "app/models.py"),
        json!({}),
    );
    // SMTP listener and the code it runs.
    let smtp = f.node(
        &format!("entry:{SERVER}@email_handler.py#main"),
        "EntryPoint",
        json!({"kind": "smtp-listener", "name": "Mail received over SMTP",
               "file": "email_handler.py", "port": 20381}),
        (SERVER, "email_handler.py"),
    );
    let main = f.code(SERVER, "email_handler.py", "main");
    f.edge(
        &smtp,
        "ENTRY",
        &main,
        (SERVER, "email_handler.py"),
        json!({}),
    );
    // An outside API the handler's code calls.
    let gap = f.node(
        &format!("gap:fetch:{SERVER}@email_handler.py@40"),
        "Gap",
        json!({"reason": "no binding evidence",
               "request": {"method": "POST", "url": "https://api.vendor.example/v1/check",
                           "url_source": "literal"}}),
        (SERVER, "email_handler.py"),
    );
    f.edge(
        &main,
        "FETCHES",
        &gap,
        (SERVER, "email_handler.py"),
        json!({}),
    );
    // Operator commands and cron jobs.
    f.node(
        &format!("entry:{SERVER}@commands/backfill.py#main"),
        "EntryPoint",
        json!({"kind": "command", "name": "Command", "file": "commands/backfill.py"}),
        (SERVER, "commands/backfill.py"),
    );
    f.node(
        &format!("sched:{SERVER}@crontab.yml#0"),
        "Schedule",
        json!({"name": "Nightly", "format": "yacron", "file": "crontab.yml",
               "schedule": "0 1 * * *"}),
        (SERVER, "crontab.yml"),
    );
    // The client: a Vue screen whose code calls the endpoint through a
    // declared service binding.
    let screen = f.node(
        &format!("screen:{CLIENT}@/aliases"),
        "Screen",
        json!({"route": "/aliases", "router": "vue-router"}),
        (CLIENT, "src/router.js"),
    );
    let call = f.code(CLIENT, "src/api.js", "fetchAliases");
    f.edge(
        &screen,
        "RENDERS",
        &call,
        (CLIENT, "src/router.js"),
        json!({}),
    );
    f.edge(
        &call,
        "FETCHES",
        &ep,
        (CLIENT, "src/api.js"),
        json!({"url": "https://app.acme.example/api/aliases", "binding": [
            {"kind": "declared-service", "service": "acme", "repo": SERVER,
             "base_url": "https://app.acme.example"}]}),
    );
    // Code with no recovered way in.
    f.code(TOOLS, "lib/util.go", "Helper");
    f
}

fn names() -> Vec<(String, String)> {
    vec![
        (SERVER.into(), "server".into()),
        (CLIENT.into(), "web-client".into()),
        (TOOLS.into(), "tools".into()),
    ]
}

fn render(f: &Fixture, mode: ExportMode, stories: Option<&Stories<'_>>) -> (String, String) {
    let nodes: Vec<&Node> = f.nodes.iter().collect();
    let edges: Vec<&Edge> = f.edges.iter().collect();
    let flows = flowtracer::trace(&f.nodes, &f.edges);
    let names = names();
    let model = Model::new(
        &nodes,
        &edges,
        &flows,
        &Supplied {
            knowledge: stories,
            repo_names: &names,
            mode,
        },
    );
    (
        markdown(&model, mode, None).unwrap().content,
        dsl(&model, None).unwrap().content,
    )
}

fn level<'t>(markdown: &'t str, heading: &str) -> &'t str {
    let start = markdown.find(heading).expect("level heading");
    let rest = &markdown[start..];
    let end = rest[3..].find("\n## ").map_or(rest.len(), |i| i + 3);
    &rest[..end]
}

/// AC-0940: containers come from endpoints, entry points, schedules and
/// client screens; stores, outside systems and people are drawn with what
/// relates them, and a client's flow into the server does not make the
/// server's stores the client's.
#[test]
fn containers_come_from_entry_points_endpoints_schedules_and_clients() {
    let (md, _) = render(&system(), ExportMode::BestEffort, None);
    let l2 = level(&md, "## Level 2");
    for expected in [
        "<b>Web application</b>",
        "[Container: Python, Flask]",
        "<b>Email handler</b>",
        "SMTP listener",
        "<b>Operator commands</b>",
        "<b>Scheduled jobs</b>",
        "<b>web-client</b>",
        "[Container: Vue]",
        "<b>SQL database</b>",
        "<b>api.vendor.example</b>",
        "<b>Mail senders</b>",
        "<b>Operator</b>",
        "<b>User</b>",
        "Calls 1 API endpoint",
        "Reads and writes 1 table",
        "subgraph sys[\"acme [Software System]\"]",
    ] {
        assert!(l2.contains(expected), "missing {expected} in\n{l2}");
    }
    // The client reaches the server's code through its call, but the SQL
    // database and the vendor API stay the server's.
    let client = l2
        .lines()
        .find(|l| l.contains("<b>web-client</b>"))
        .and_then(|l| l.trim().split('[').next())
        .unwrap()
        .to_string();
    for line in l2
        .lines()
        .filter(|l| l.trim_start().starts_with(&format!("{client} -")))
    {
        assert!(
            line.contains("Calls 1 API endpoint"),
            "client draws only its call: {line}"
        );
    }
    // Level 1 folds every container into the system box.
    let l1 = level(&md, "## Level 1");
    assert!(l1.contains("sys[\"<b>acme</b><br/><i>[Software System]</i>"));
    assert!(!l1.contains("<b>Web application</b>"));
    assert!(l1.contains("sys -->|\"Calls"));
}

/// AC-0940: a repository with code and no endpoint, entry point or screen
/// is drawn as a Gap container, never left out.
#[test]
fn a_repository_with_no_way_in_is_a_gap_container() {
    let (md, _) = render(&system(), ExportMode::BestEffort, None);
    let line = md
        .lines()
        .find(|l| l.contains("<b>tools</b>"))
        .expect("tools container");
    assert!(line.ends_with(":::gap"));
    assert!(md.contains(
        "| tools | Container | Go | 1 file; no process start, endpoint or screen recovered | Gap |"
    ));
}

/// AC-0940: `verified-only` draws no InferredWeak person or relation, and
/// says why it draws no component.
#[test]
fn verified_only_draws_nothing_inferred_weak() {
    let (md, _) = render(&system(), ExportMode::VerifiedOnly, None);
    assert!(!md.contains("<b>User</b>"));
    assert!(!md.contains("-.->|\"Uses"));
    assert!(md.contains("a `verified-only` export admits none"));
    let (best, _) = render(&system(), ExportMode::BestEffort, None);
    assert!(best.contains("<b>User</b>"));
}

/// AC-0940: the same facts in any order, handed over twice, give the same
/// bytes, and every assertion resolves in the fact index.
#[test]
fn two_exports_are_byte_identical_whatever_the_input_order() {
    let f = system();
    let flows = flowtracer::trace(&f.nodes, &f.edges);
    let first = compile_spec(
        &f.nodes,
        &f.edges,
        &flows,
        ExportMode::BestEffort,
        &BTreeSet::new(),
    );
    let mut nodes: Vec<Node> = f.nodes.iter().rev().cloned().collect();
    nodes.extend(f.nodes.iter().take(4).cloned());
    let mut edges: Vec<Edge> = f.edges.iter().rev().cloned().collect();
    edges.rotate_left(3);
    let reversed: Vec<_> = flows.iter().rev().cloned().collect();
    let second = compile_spec(
        &nodes,
        &edges,
        &reversed,
        ExportMode::BestEffort,
        &BTreeSet::new(),
    );
    for id in [MARKDOWN_ID, DSL_ID] {
        let a = first.artifacts.iter().find(|a| a.id == id).unwrap();
        let b = second.artifacts.iter().find(|a| a.id == id).unwrap();
        assert_eq!(a, b, "{id}");
    }
    let artifact = first
        .artifacts
        .iter()
        .find(|a| a.id == MARKDOWN_ID)
        .unwrap();
    assert!(!artifact.assertions.is_empty());
    let index_json = first
        .artifacts
        .iter()
        .find(|a| a.id == crate::workbench::fact_index::JSON_ID)
        .unwrap();
    let index: FactIndex = serde_json::from_str(&index_json.content).unwrap();
    let cited: Vec<String> = index
        .rows
        .iter()
        .filter(|row| row.cited_by.iter().any(|by| by == MARKDOWN_ID))
        .map(|row| row.citation_id.clone())
        .collect();
    assert!(!cited.is_empty());
    check_citations(&index, cited.iter().map(String::as_str)).expect("every id resolves");
}

/// AC-0941: components are the areas of the files a container owns, with
/// calls between them and a client's call entering at its handler's area.
#[test]
fn components_are_the_areas_of_the_files_a_container_owns() {
    let f = system();
    let area = |name: &str, terms: &[&str]| Node {
        id: format!("subsystem:{name}"),
        label: "Subsystem".into(),
        props: json!({"name": name, "kind": "cluster", "terms": terms,
                      "prov": prov(Tier::Semantic, ConfidenceTier::InferredWeak,
                                   (SERVER, "."), name)}),
    };
    let api = area("Alias", &["alias", "api"]);
    let models = area("Data model", &["model"]);
    let api_file = format!("file:{SERVER}@app/api/alias.py");
    let model_file = format!("file:{SERVER}@app/models.py");
    let stories = Stories {
        included: Vec::new(),
        filtered: 0,
        areas: BTreeMap::new(),
        file_areas: [(api_file.as_str(), &api), (model_file.as_str(), &models)]
            .into_iter()
            .collect(),
    };
    let (md, dsl) = render(&f, ExportMode::BestEffort, Some(&stories));
    let l3 = level(&md, "## Level 3");
    let web = &l3[l3.find("### Web application").unwrap()..];
    let web = &web[..web[4..].find("\n### ").map_or(web.len(), |i| i + 4)];
    assert!(web.contains("subgraph box[\"Web application [Container]\"]"));
    assert!(web.contains("<b>Alias</b><br/><i>[Component]</i>"));
    assert!(web.contains("<b>Data model</b>"));
    assert!(web.contains("Calls 1 function"), "{web}");
    assert!(
        web.contains("| web-client | Calls 1 API endpoint | Alias |"),
        "{web}"
    );
    assert!(web.contains("| Data model | Reads and writes 1 table | SQL database |"));
    assert!(dsl.contains("= component \"Alias\""), "{dsl}");
    assert!(dsl.contains("component e0 \"Components-0\""), "{dsl}");
}

/// AC-0942: the DSL workspace holds the system, every container with its
/// technology, the people and outside systems, every relationship and a
/// view per level, each string quoted.
#[test]
fn the_dsl_workspace_holds_every_element_and_relationship() {
    let (_, dsl) = render(&system(), ExportMode::BestEffort, None);
    assert!(dsl.starts_with("/*"));
    assert!(dsl.contains("workspace \"acme\""));
    assert!(dsl.contains("sys = softwareSystem \"acme\" {"));
    assert!(dsl.contains("container \"Web application\""));
    assert!(dsl.contains("\"Python, Flask\""));
    assert!(dsl.contains("container \"SQL database\""));
    assert!(dsl.contains("\"Database\""));
    assert!(dsl.contains("person \"Operator\""));
    assert!(dsl.contains("softwareSystem \"api.vendor.example\""));
    assert!(dsl.contains("\"Calls 1 API endpoint\" \"HTTPS\" \"Relationship\" {"));
    // A relationship keeps its confidence, and an inferred one is styled
    // apart from an evidenced one.
    assert!(
        dsl.contains("\"Uses\" \"\" \"Relationship,InferredWeak\" {"),
        "{dsl}"
    );
    assert!(dsl.contains("\"Relationship,Gap\""));
    assert!(dsl.contains("relationship \"InferredWeak\" {"));
    assert!(dsl.contains("systemContext sys \"SystemContext\""));
    assert!(dsl.contains("container sys \"Containers\""));
    assert!(dsl.contains("\"confidence\" \"Gap\""));
    let opened = dsl.matches('{').count();
    assert_eq!(opened, dsl.matches('}').count(), "balanced braces");
}

/// AC-0940: only a resource that holds data is a store, and a store is
/// scoped to its repository: two repositories reading the same variable
/// name keep two stores, told apart by repository.
#[test]
fn stores_hold_data_and_stay_in_their_repository() {
    let mut f = system();
    f.node(
        &format!("res:{SERVER}@aws_instance.web"),
        "Resource",
        json!({"kind": "iac", "type": "aws_instance", "logical_id": "aws_instance.web"}),
        (SERVER, "main.tf"),
    );
    let db = f.node(
        &format!("res:{CLIENT}@observed:sql-database:env:DB_URI"),
        "Resource",
        json!({"kind": "sdk-client", "provider": "sql-database", "module": "sqlalchemy",
               "observed": true, "env_key": "DB_URI",
               "logical_id": "observed:sql-database:env:DB_URI"}),
        (CLIENT, "db.py"),
    );
    let file = format!("file:{CLIENT}@db.py");
    f.node(&file, "File", json!({"path": "db.py"}), (CLIENT, "db.py"));
    f.edge(&file, "USES", &db, (CLIENT, "db.py"), json!({}));
    let (md, _) = render(&f, ExportMode::BestEffort, None);
    assert!(!md.contains("aws_instance"), "{md}");
    assert!(
        md.contains("| SQL database (server) | Data store |"),
        "{md}"
    );
    assert!(
        md.contains("| SQL database (web-client) | Data store |"),
        "{md}"
    );
    assert!(md.contains("Data stores: 2."));
}

/// AC-0943: the app's Architecture view is the model `architecture.md` was
/// drawn from: the same element ids and names, bound to the artifact's
/// identity, and every fact it shows is a graph fact with that fact's
/// provenance.
#[test]
fn the_app_view_is_the_model_the_markdown_draws() {
    let f = system();
    let nodes: Vec<&Node> = f.nodes.iter().collect();
    let edges: Vec<&Edge> = f.edges.iter().collect();
    let flows = flowtracer::trace(&f.nodes, &f.edges);
    let names = names();
    let model = Model::new(
        &nodes,
        &edges,
        &flows,
        &Supplied {
            knowledge: None,
            repo_names: &names,
            mode: ExportMode::BestEffort,
        },
    );
    let output = markdown(&model, ExportMode::BestEffort, None).unwrap();
    let mut artifact = crate::SpecArtifact {
        id: "architecture".into(),
        file_name: "architecture.md".into(),
        title: "Architecture".into(),
        format: "markdown".into(),
        schema: crate::SpecArtifact::expected_schema("architecture"),
        content_id: String::new(),
        content: output.content,
        assertions: output.assertions,
    };
    artifact.content_id = artifact.compute_content_id();
    let view = view(&model, &artifact, ExportMode::BestEffort);
    assert_eq!(view.artifact_id, artifact.id);
    assert_eq!(view.schema, artifact.schema);
    assert_eq!(view.content_id, artifact.content_id);
    assert_eq!(view.system, "acme");
    assert!(!view.elements.is_empty());
    for element in &view.elements {
        let drawn = artifact
            .content
            .lines()
            .find(|line| {
                let line = line.trim_start();
                line.starts_with(&format!("{}[", element.id))
                    || line.starts_with(&format!("{}(", element.id))
            })
            .unwrap_or_else(|| panic!("{} is drawn", element.id));
        assert!(
            drawn.contains(&element.name),
            "{drawn} names {}",
            element.name
        );
        assert!(element.facts.len() <= MAX_CITED);
        assert!(element.facts_total >= element.facts.len());
    }
    assert_eq!(view.relations.len(), model.relations.len());
    let ids: Vec<&str> = view.elements.iter().map(|e| e.id.as_str()).collect();
    for relation in &view.relations {
        assert!(ids.contains(&relation.from.as_str()) && ids.contains(&relation.to.as_str()));
    }
    let facts = view
        .elements
        .iter()
        .flat_map(|e| &e.facts)
        .chain(view.relations.iter().flat_map(|r| &r.facts));
    let mut seen = 0;
    for fact in facts {
        seen += 1;
        let props = f
            .nodes
            .iter()
            .find(|n| n.id == fact.id)
            .map(|n| &n.props)
            .or_else(|| {
                f.edges
                    .iter()
                    .find(|e| crate::workbench::edge_identity(e) == fact.id)
                    .map(|e| &e.props)
            })
            .unwrap_or_else(|| panic!("{} is a graph fact", fact.id));
        assert_eq!(
            serde_json::to_value(fact.provenance.confidence_tier).unwrap(),
            props["prov"]["confidence_tier"],
            "{}",
            fact.id
        );
    }
    assert!(seen > 0);
}

/// AC-0948: a Cloudflare Worker entry point is its own container, named for
/// the worker and described by the module it runs and the route it serves.
#[test]
fn an_edge_worker_is_a_container_named_for_the_worker() {
    let mut f = system();
    let entry = f.node(
        &format!("entry:{TOOLS}@edge/wrangler.jsonc#worker"),
        "EntryPoint",
        json!({"kind": "edge-worker", "name": "Edge worker (edge/wrangler.jsonc)",
               "how": "Cloudflare Worker", "file": "edge/wrangler.jsonc",
               "worker": "agent-events", "route": "events.example.com",
               "webhook_from": "GitHub",
               "configured_urls": {"SERVER_URL": "https://api.example.com",
                                   "LIST_URL": "https://api.example.com/api/aliases?x=1",
                                   "OTHER": "https://elsewhere.example.com/nothing"}}),
        (TOOLS, "edge/wrangler.jsonc"),
    );
    let module = format!("file:{TOOLS}@edge/src/index.ts");
    f.node(
        &module,
        "File",
        json!({"path": "edge/src/index.ts"}),
        (TOOLS, "edge/src/index.ts"),
    );
    f.edge(
        &entry,
        "ENTRY",
        &module,
        (TOOLS, "edge/wrangler.jsonc"),
        json!({}),
    );
    let (md, _) = render(&f, ExportMode::BestEffort, None);
    let l2 = level(&md, "## Level 2");
    assert!(l2.contains("<b>Agent events</b>"), "{l2}");
    assert!(
        l2.contains("[Container: TypeScript, Cloudflare Worker]"),
        "{l2}"
    );
    assert!(
        l2.contains("Runs `edge/src/index.ts` on Cloudflare's edge; serves `events.example.com`"),
        "{l2}"
    );
    // GitHub sends it webhooks; the configured URL naming the web app's
    // endpoint is an inferred call, the bare origin and the unknown path
    // draw nothing.
    assert!(l2.contains("<b>GitHub</b>"), "{l2}");
    assert!(
        md.contains("| GitHub | Sends webhooks to | Agent events | HTTPS | Confirmed |"),
        "{md}"
    );
    assert!(
        md.contains("| Agent events | Calls | Web application | HTTPS | InferredWeak |"),
        "{md}"
    );
    assert_eq!(md.matches("| Agent events | Calls |").count(), 1, "{md}");
    let (strict, _) = render(&f, ExportMode::VerifiedOnly, None);
    assert!(!strict.contains("| Agent events | Calls |"), "{strict}");
}

/// AC-0948: a configured URL matches an endpoint route by its segments.
#[test]
fn a_configured_url_matches_an_endpoint_route() {
    assert_eq!(url_path("https://a.example/x/y?q=1"), Some("/x/y"));
    assert_eq!(url_path("https://a.example"), None);
    assert_eq!(url_path("https://a.example/"), None);
    assert!(path_matches("/api/hooks/{path...}", "/api/hooks/batch"));
    assert!(path_matches("/api/hooks/{path...}", "/api/hooks/a/b"));
    assert!(!path_matches("/api/hooks/{path...}", "/api/hooks"));
    assert!(path_matches("/users/:id", "/users/7"));
    assert!(!path_matches("/users/:id", "/users/7/x"));
    assert!(path_matches("/.well-known/k.json", "/.well-known/k.json"));
}

/// AC-0947: a store known only by its URL variable, read in one place with
/// a fallback variable of the same provider, is drawn once.
#[test]
fn a_store_read_through_a_fallback_variable_is_one_store() {
    let mut f = system();
    let reader = f.code(SERVER, "app/kv.py", "from_env");
    for env in ["KV_REST_API_URL", "UPSTASH_REDIS_REST_URL"] {
        let store = f.node(
            &format!("res:{SERVER}@observed:redis:env:{env}"),
            "Resource",
            json!({"kind": "sdk-client", "provider": "redis", "observed": true,
                   "env_key": env, "store_from_env_key": true,
                   "logical_id": format!("observed:redis:env:{env}")}),
            (SERVER, "app/kv.py"),
        );
        f.edge(&reader, "USES", &store, (SERVER, "app/kv.py"), json!({}));
    }
    let (md, _) = render(&f, ExportMode::BestEffort, None);
    let l2 = level(&md, "## Level 2");
    assert_eq!(l2.matches("<b>Redis</b>").count(), 1, "{l2}");
    assert!(
        l2.contains("Located by `KV_REST_API_URL` or `UPSTASH_REDIS_REST_URL`"),
        "{l2}"
    );
}
