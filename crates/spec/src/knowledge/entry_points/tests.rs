use super::*;
use core_graph::{Edge, StoreRevision};

const REPO: &str = "repo";
const COMMIT: &str = "2cc309deb5394af176314552ac7320a7b26a0104";

fn prov(
    extractor: &str,
    path: &str,
    confidence: ConfidenceTier,
    identity: &str,
) -> serde_json::Value {
    let evidence = vec![EvidenceRef {
        repo: REPO.into(),
        path: path.into(),
        byte_start: 0,
        byte_end: 10,
        commit_sha: COMMIT.into(),
    }];
    serde_json::to_value(
        Provenance::new(
            Tier::Deterministic,
            confidence,
            evidence,
            extractor,
            identity.as_bytes(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn entry(file: &str, key: &str, kind: &str, how: &str, dst: &str) -> (Node, Edge) {
    let id = format!("entry:{REPO}@{file}#{key}");
    let node = Node {
        id: id.clone(),
        label: "EntryPoint".into(),
        props: serde_json::json!({
            "name": format!("{kind} ({file})"),
            "kind": kind,
            "how": how,
            "file": file,
            "prov": prov("t0.entry-point", file, ConfidenceTier::Confirmed, &id),
        }),
    };
    let edge = Edge {
        src: id.clone(),
        dst: dst.into(),
        label: "ENTRY".into(),
        props: serde_json::json!({
            "prov": prov("t0.entry-point", file, ConfidenceTier::Confirmed, dst),
        }),
    };
    (node, edge)
}

fn file(path: &str) -> Node {
    Node {
        id: format!("file:{REPO}@{path}"),
        label: "File".into(),
        props: serde_json::json!({"path": path}),
    }
}

fn schedule(file: &str, index: usize, format: &str) -> Node {
    let id = format!("sched:{REPO}@{file}#{index}");
    Node {
        id: id.clone(),
        label: "Schedule".into(),
        props: serde_json::json!({
            "name": format!("job {index}"),
            "file": file,
            "format": format,
            "prov": prov("t0.schedule", file, ConfidenceTier::Confirmed, &id),
        }),
    }
}

fn snapshot() -> RevisionedSnapshot {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for (path, key, kind, how) in [
        ("email_handler.py", "main", "smtp-listener", "__main__"),
        ("job_runner.py", "main", "polling-loop", "__main__"),
        ("monitoring.py", "main", "polling-loop", "__main__"),
        ("commands/check_leaks.py", "main", "command", "__main__"),
        ("commands/backfill.py", "main", "command", "script"),
    ] {
        let (node, edge) = entry(path, key, kind, how, &format!("file:{REPO}@{path}"));
        nodes.extend([node, file(path)]);
        edges.push(edge);
    }
    let gap = Node {
        id: format!("gap:entry-point:{REPO}@Dockerfile#cmd"),
        label: "Gap".into(),
        props: serde_json::json!({
            "reason": "entry point not interpreted: shell-composed command",
            "prov": prov("t0.entry-point", "Dockerfile", ConfidenceTier::Gap, "gap"),
        }),
    };
    let (node, edge) = entry("Dockerfile", "cmd", "process", "container command", &gap.id);
    nodes.extend([node, gap]);
    edges.push(edge);
    nodes.extend([
        schedule("crontab.yml", 0, "yacron"),
        schedule("crontab.yml", 1, "yacron"),
        schedule("crontab-all-hosts.yml", 0, "yacron"),
        schedule(".github/workflows/nightly.yml", 0, "github-actions"),
    ]);
    RevisionedSnapshot {
        revision: StoreRevision {
            graph_instance: "fixture".into(),
            generation: 1,
        },
        graph: (nodes, edges),
    }
}

fn capabilities(report: &CapabilityReport) -> BTreeMap<String, BTreeSet<String>> {
    report
        .facts
        .iter()
        .map(|fact| {
            let OverlayItem::Node(node) = &fact.item else {
                panic!("Capability is a node")
            };
            assert_eq!(node.label, "Capability");
            let p: Provenance = serde_json::from_value(node.props["prov"].clone()).unwrap();
            assert_eq!(p.tier, Tier::Semantic);
            assert_eq!(p.confidence_tier, ConfidenceTier::InferredWeak);
            assert_eq!(
                p.method,
                ProducingMethod::Rule {
                    rule: CAPABILITY_ENTRY_POINT_RULE.into(),
                    version: CAPABILITY_ENTRY_POINT_RULE_VERSION.into()
                }
            );
            let cites = fact
                .cites
                .iter()
                .map(|cite| match cite {
                    BaseRef::Node(id) => id.clone(),
                    BaseRef::Edge(..) => panic!("cites nodes only"),
                })
                .collect();
            (node.props["title"].as_str().unwrap().to_owned(), cites)
        })
        .collect()
}

// AC-0893: every recovered entry point is under a capability: one per
// process start, operator commands grouped by their directory, and each
// schedule file's jobs together. An entry point whose ENTRY is a Gap and a
// scheduled CI workflow are under none; the Gap is in the gap register.
#[test]
fn entry_points_and_schedules_are_capabilities() {
    let report = derive_entry_point_capabilities(&snapshot());
    let caps = capabilities(&report);
    let ids = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
    assert_eq!(
        caps,
        BTreeMap::from([
            (
                "Receive mail over SMTP".into(),
                ids(&["entry:repo@email_handler.py#main"])
            ),
            (
                "Run the job runner loop".into(),
                ids(&["entry:repo@job_runner.py#main"])
            ),
            (
                "Run the monitoring loop".into(),
                ids(&["entry:repo@monitoring.py#main"])
            ),
            (
                "Run operator commands".into(),
                ids(&[
                    "entry:repo@commands/backfill.py#main",
                    "entry:repo@commands/check_leaks.py#main"
                ])
            ),
            (
                "Run scheduled jobs (crontab)".into(),
                ids(&["sched:repo@crontab.yml#0", "sched:repo@crontab.yml#1"])
            ),
            (
                "Run scheduled jobs (crontab all hosts)".into(),
                ids(&["sched:repo@crontab-all-hosts.yml#0"])
            ),
        ])
    );
    // Deterministic identity: the same base yields the same facts.
    assert_eq!(report, derive_entry_point_capabilities(&snapshot()));
}

// AC-0893: support from another extractor, or a second ENTRY, proves no
// capability.
#[test]
fn unproven_entry_points_are_not_capabilities() {
    let mut base = snapshot();
    for node in &mut base.graph.0 {
        if node.id == "entry:repo@job_runner.py#main" {
            node.props["prov"] = prov("t0.other", "job_runner.py", ConfidenceTier::Confirmed, "x");
        }
    }
    let (_, second) = entry(
        "monitoring.py",
        "main",
        "polling-loop",
        "__main__",
        "file:repo@job_runner.py",
    );
    base.graph.1.push(second);
    let caps = capabilities(&derive_entry_point_capabilities(&base));
    assert!(
        !caps
            .keys()
            .any(|t| t.contains("job runner") || t.contains("monitoring"))
    );
    assert!(caps.contains_key("Receive mail over SMTP"));
}

// AC-0893 review (#2011): two processes of one Procfile with the same kind
// are told apart by their key.
#[test]
fn colliding_titles_in_one_file_name_their_entry() {
    let mut base = snapshot();
    for key in ["web", "api"] {
        let (node, edge) = entry(
            "Procfile",
            key,
            "http-server",
            "Procfile process",
            "file:repo@email_handler.py",
        );
        base.graph.0.push(node);
        base.graph.1.push(edge);
    }
    let caps = capabilities(&derive_entry_point_capabilities(&base));
    assert!(
        caps.contains_key("Serve HTTP requests (web process)"),
        "{caps:?}"
    );
    assert!(
        caps.contains_key("Serve HTTP requests (API process)"),
        "{caps:?}"
    );
}

// AC-0921: a Node script's subcommands are one operator-command capability
// of that script, apart from an operator directory's commands.
#[test]
fn a_scripts_subcommands_are_one_capability() {
    let mut base = snapshot();
    for key in ["run", "status"] {
        let path = "hq/main.mjs";
        let (node, edge) = entry(
            path,
            key,
            "command",
            "subcommand",
            &format!("file:{REPO}@{path}"),
        );
        base.graph.0.push(node);
        base.graph.1.push(edge);
    }
    base.graph.0.push(file("hq/main.mjs"));
    let caps = capabilities(&derive_entry_point_capabilities(&base));
    let groups: Vec<(&String, &BTreeSet<String>)> = caps
        .iter()
        .filter(|(title, _)| title.starts_with("Run operator commands"))
        .collect();
    assert_eq!(groups.len(), 2, "{caps:#?}");
    assert!(groups.iter().any(|(_, ids)| ids
        == &&BTreeSet::from([
            "entry:repo@hq/main.mjs#run".to_string(),
            "entry:repo@hq/main.mjs#status".to_string()
        ])));
}

// AC-0966: a Java class's `@Scheduled` methods are one scheduled-jobs
// capability, and a startup runner is titled by its class in words.
#[test]
fn java_scheduled_methods_are_one_capability_and_runners_run_at_startup() {
    let mut base = snapshot();
    let jobs = "src/main/java/com/acme/ReportJobs.java";
    let loader = "src/main/java/com/acme/DataLoader.java";
    for (path, key, kind, how) in [
        (jobs, "ReportJobs.purge", "scheduled-job", "@Scheduled"),
        (jobs, "ReportJobs.mail", "scheduled-job", "@Scheduled"),
        (
            loader,
            "DataLoader.run",
            "startup-task",
            "CommandLineRunner",
        ),
    ] {
        let (node, edge) = entry(path, key, kind, how, &format!("file:{REPO}@{path}"));
        base.graph.0.push(node);
        base.graph.1.push(edge);
    }
    base.graph.0.extend([file(jobs), file(loader)]);
    let caps = capabilities(&derive_entry_point_capabilities(&base));
    let scheduled = caps
        .iter()
        .find(|(_, ids)| ids.contains(&format!("entry:{REPO}@{jobs}#ReportJobs.purge")))
        .unwrap_or_else(|| panic!("{caps:#?}"));
    assert!(scheduled.0.starts_with("Run scheduled jobs"), "{caps:#?}");
    assert_eq!(scheduled.1.len(), 2);
    assert!(
        caps.get("Run data loader at startup")
            .is_some_and(|ids| ids.contains(&format!("entry:{REPO}@{loader}#DataLoader.run"))),
        "{caps:#?}"
    );
}

// AC-1110: an Android component or a WorkManager worker is titled by the
// class it names, in words, not by the manifest declaring it.
#[test]
fn android_components_are_titled_by_their_class() {
    let mut base = snapshot();
    let manifest = "app/src/main/AndroidManifest.xml";
    let main = "app/src/main/java/com/acme/MainActivity.kt";
    let boot = "app/src/main/java/com/acme/BootReceiver.kt";
    let worker = "app/src/main/java/com/acme/SyncWorker.kt";
    for (path, key, kind, component, dst) in [
        (
            manifest,
            "activity:com.acme.MainActivity",
            "android-activity",
            "com.acme.MainActivity",
            main,
        ),
        (
            manifest,
            "receiver:com.acme.BootReceiver",
            "android-receiver",
            "com.acme.BootReceiver",
            boot,
        ),
        (
            worker,
            "SyncWorker.doWork",
            "background-job",
            "SyncWorker",
            worker,
        ),
    ] {
        let (mut node, edge) = entry(path, key, kind, "x", &format!("file:{REPO}@{dst}"));
        node.props["component"] = component.into();
        base.graph.0.push(node);
        base.graph.1.push(edge);
    }
    base.graph.0.extend([file(main), file(boot), file(worker)]);
    let caps = capabilities(&derive_entry_point_capabilities(&base));
    for title in [
        "Open the main activity screen",
        "Receive broadcasts in boot receiver",
        "Run sync worker in the background",
    ] {
        assert!(caps.contains_key(title), "{title}: {caps:#?}");
    }
}
