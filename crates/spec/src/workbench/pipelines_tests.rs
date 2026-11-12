//! `pipelines.md` (#1818, AC-0650).
use super::super::selected;
use super::*;
use crate::{
    FactIndex, FactIndexError, SelectedArtifactOutcome, SelectedCompileLimits, SpecArtifact,
    check_citations, citation_id, compile_spec, compile_spec_selected_with_coverage,
    compile_spec_with_coverage, try_compile_spec_with_coverage,
};
use core_graph::FactKey;
use core_graph::coverage::CoverageRecord;
use core_prov::{EvidenceRef, Tier};
use serde_json::{Value, json};

const ID: &str = "pipelines";
const REPO: &str = "acme/shop";
const WORKFLOW: &str = ".github/workflows/deploy.yml";
const SPAN_PREFIX: &str = "acme/shop:.github/workflows/deploy.yml bytes";

fn prov(tier: Tier, confidence: ConfidenceTier, path: &str, span: (u64, u64), fact: &str) -> Value {
    let evidence = EvidenceRef {
        repo: REPO.into(),
        path: path.into(),
        byte_start: span.0,
        byte_end: span.1,
        commit_sha: "0123abc".into(),
    };
    serde_json::to_value(
        Provenance::new(
            tier,
            confidence,
            vec![evidence],
            "t0.pipeline",
            fact.as_bytes(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn confirmed(path: &str, span: (u64, u64), fact: &str) -> Value {
    prov(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        path,
        span,
        fact,
    )
}

fn weak_prov(path: &str, span: (u64, u64), fact: &str) -> Value {
    prov(
        Tier::Agentic,
        ConfidenceTier::InferredWeak,
        path,
        span,
        fact,
    )
}

/// Pipeline facts in the shape `ingest::pipelines` emits them.
#[derive(Clone, Default)]
struct Fixture {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Fixture {
    fn node(&mut self, id: String, label: &str, path: &str, mut props: Value, span: (u64, u64)) {
        props["file"] = path.into();
        props["prov"] = confirmed(path, span, &id);
        self.nodes.push(Node {
            id,
            label: label.into(),
            props,
        });
    }

    fn part_of(&mut self, src: &str, dst: &str, path: &str, index: u64, span: (u64, u64)) {
        self.edges.push(Edge {
            src: src.into(),
            dst: dst.into(),
            label: "PART_OF".into(),
            props: json!({
                "index": index,
                "prov": confirmed(path, span, &format!("{src} PART_OF {dst}")),
            }),
        });
    }

    fn pipeline(&mut self, path: &str, props: Value, span: (u64, u64)) -> String {
        let id = format!("pipeline:{REPO}@{path}");
        self.node(id.clone(), "Pipeline", path, props, span);
        id
    }

    fn trigger(&mut self, path: &str, event: &str, index: u64, span: (u64, u64)) {
        let id = format!("pipeline-trigger:{REPO}@{path}#{event}");
        let props = json!({"event": event, "index": index});
        self.node(id.clone(), "PipelineTrigger", path, props, span);
        self.part_of(&id, &format!("pipeline:{REPO}@{path}"), path, index, span);
    }

    fn job(&mut self, path: &str, key: &str, index: u64, mut props: Value, span: (u64, u64)) {
        let id = job_id(path, key);
        props["index"] = index.into();
        self.node(id.clone(), "PipelineJob", path, props, span);
        self.part_of(&id, &format!("pipeline:{REPO}@{path}"), path, index, span);
    }

    fn step(&mut self, path: &str, job: &str, index: u64, mut props: Value, span: (u64, u64)) {
        let id = step_id(path, job, index);
        props["index"] = index.into();
        self.node(id.clone(), "PipelineStep", path, props, span);
        self.part_of(&id, &job_id(path, job), path, index, span);
    }

    fn needs(&mut self, path: &str, from: &str, to: &str, via: &str, span: (u64, u64)) {
        let (src, dst) = (job_id(path, from), job_id(path, to));
        self.edges.push(Edge {
            props: json!({
                "via": [via],
                "prov": confirmed(path, span, &format!("{src} NEEDS {dst}")),
            }),
            src,
            dst,
            label: "NEEDS".into(),
        });
    }

    /// A `needs` naming a job the workflow does not declare: the reader's
    /// explicit Gap node and Gap relation (R-INT-4).
    fn dangling(&mut self, path: &str, from: &str, name: &str, span: (u64, u64)) {
        const REASON: &str = "`needs` names a job this workflow does not declare";
        let src = job_id(path, from);
        let gap = format!("gap:pipeline:{REPO}@{path}#{from}/needs/{name}");
        let gap_prov =
            |fact: &str| prov(Tier::Deterministic, ConfidenceTier::Gap, path, span, fact);
        self.nodes.push(Node {
            id: gap.clone(),
            label: "Gap".into(),
            props: json!({
                "reason": REASON, "needs": name, "attempted_tiers": ["T0"],
                "prov": gap_prov(&gap),
            }),
        });
        self.edges.push(Edge {
            props: json!({
                "reason": REASON, "via": ["needs"],
                "prov": gap_prov(&format!("{src} NEEDS {gap}")),
            }),
            src,
            dst: gap,
            label: "NEEDS".into(),
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
        .expect("the pipeline artifact is exported")
}

fn job_id(path: &str, key: &str) -> String {
    format!("pipeline-job:{REPO}@{path}#{key}")
}

fn step_id(path: &str, job: &str, index: u64) -> String {
    format!("pipeline-step:{REPO}@{path}#{job}/{index}")
}

/// A workflow whose jobs are declared against their dependency order
/// (deploy, test, build, lint), and a two-stage Dockerfile. Facts are added
/// out of declared order.
fn deploy() -> Fixture {
    let mut f = Fixture::default();
    f.pipeline(
        WORKFLOW,
        json!({"format": "github-actions", "name": "Deploy"}),
        (0, 12),
    );
    f.trigger(WORKFLOW, "workflow_dispatch", 1, (40, 57));
    f.trigger(WORKFLOW, "push", 0, (20, 39));
    f.job(
        WORKFLOW,
        "lint",
        3,
        json!({"kind": "job", "id": "lint"}),
        (400, 440),
    );
    f.job(
        WORKFLOW,
        "build",
        2,
        json!({"kind": "job", "id": "build", "runs_on": "ubuntu-latest"}),
        (300, 340),
    );
    f.job(
        WORKFLOW,
        "test",
        1,
        json!({"kind": "job", "id": "test"}),
        (200, 240),
    );
    f.job(
        WORKFLOW,
        "deploy",
        0,
        json!({"kind": "job", "id": "deploy", "environment": "production"}),
        (100, 140),
    );
    f.needs(WORKFLOW, "deploy", "test", "needs", (112, 116));
    f.needs(WORKFLOW, "deploy", "build", "needs", (106, 111));
    f.needs(WORKFLOW, "test", "build", "needs", (210, 215));
    f.step(
        WORKFLOW,
        "build",
        1,
        json!({"kind": "run", "name": "Compile", "run": "npm ci | tee log\nnpm run build"}),
        (320, 340),
    );
    f.step(
        WORKFLOW,
        "build",
        0,
        json!({"kind": "uses", "uses": "actions/checkout@v4"}),
        (310, 319),
    );
    f.step(
        WORKFLOW,
        "deploy",
        0,
        json!({"kind": "run", "run": "./deploy.sh", "secrets": ["DEPLOY_TOKEN"]}),
        (120, 140),
    );

    f.pipeline(
        "Dockerfile",
        json!({"format": "dockerfile", "stages": 2, "final_stage": "stage-1"}),
        (0, 20),
    );
    f.job(
        "Dockerfile",
        "stage-1",
        1,
        json!({"kind": "stage", "base_image": "nginx:1.27", "final": true}),
        (60, 75),
    );
    f.job(
        "Dockerfile",
        "stage-0",
        0,
        json!({"kind": "stage", "base_image": "node:20", "name": "build"}),
        (0, 20),
    );
    f.needs("Dockerfile", "stage-1", "stage-0", "copy-from", (76, 110));
    f.step(
        "Dockerfile",
        "stage-0",
        0,
        json!({"kind": "instruction", "instruction": "FROM", "text": "FROM node:20 AS build"}),
        (0, 20),
    );
    f
}

fn line<'a>(content: &'a str, start: &str) -> &'a str {
    content
        .lines()
        .find(|line| line.starts_with(start))
        .unwrap_or_else(|| panic!("no row starting {start:?} in:\n{content}"))
}

/// The lines of one pipeline's `### heading` table.
fn table<'a>(content: &'a str, pipeline: &str, heading: &str) -> Vec<&'a str> {
    let start = content
        .find(&format!("\n## Pipeline {pipeline}\n"))
        .unwrap_or_else(|| panic!("no pipeline section {pipeline}"));
    let body = &content[start..];
    let body = &body[body.find(&format!("\n### {heading}\n")).unwrap()..];
    body.lines()
        .skip(1)
        .skip_while(|line| !line.starts_with("|---"))
        .skip(1)
        .take_while(|line| line.starts_with('|'))
        .collect()
}

const WORKFLOW_SECTION: &str = "acme/shop@.github/workflows/deploy.yml";

// AC-0650: each pipeline lists its triggers in declared order, its jobs
// in dependency order with the declared position breaking ties, and each
// job's steps in declared order, with tier, confidence and span on each row.
#[test]
fn jobs_are_in_dependency_order_and_steps_in_declared_order() {
    let artifact = deploy().artifact(ExportMode::VerifiedOnly);
    assert_eq!(artifact.file_name, "pipelines.md");
    let content = &artifact.content;
    assert!(content.contains(
        "Export mode: `verified-only`. Pipelines: 2. Triggers: 2. Jobs: 6, of which not \
         ordered: 0. Steps: 4."
    ));

    let summary = line(content, "| acme/shop@.github/workflows/deploy.yml |");
    assert!(summary.contains("| github-actions | name: Deploy | 2 | 4 | 3 |"));
    assert!(summary.contains(&format!(
        "| Deterministic | Confirmed | {SPAN_PREFIX} 0..12 @ 0123abc | — |"
    )));

    let triggers = table(content, WORKFLOW_SECTION, "Triggers");
    assert_eq!(triggers.len(), 2);
    assert!(triggers[0].starts_with("| 0 | push | — | Deterministic | Confirmed |"));
    assert!(triggers[1].starts_with("| 1 | workflow\\_dispatch |"));

    // build is needed by test and deploy; lint needs nothing and is declared
    // last, so it follows every job that became ready before it.
    let jobs = table(content, WORKFLOW_SECTION, "Jobs in dependency order");
    let cells: Vec<Vec<&str>> = jobs
        .iter()
        .map(|row| row.split(" | ").take(5).collect())
        .collect();
    assert_eq!(
        cells,
        [
            ["| 1", "build", "job", "2", "—"],
            ["| 2", "test", "job", "1", "build (via needs)"],
            [
                "| 3",
                "deploy",
                "job",
                "0",
                "build (via needs); test (via needs)"
            ],
            ["| 4", "lint", "job", "3", "—"],
        ]
    );
    assert!(jobs[0].ends_with(&format!(
        "| runs-on: ubuntu-latest | Deterministic | Confirmed | {SPAN_PREFIX} 300..340 @ \
         0123abc | — |"
    )));
    assert!(jobs[2].contains("| environment: production |"));

    let steps = table(content, WORKFLOW_SECTION, "Steps in declared order");
    assert_eq!(steps.len(), 3);
    assert!(steps[0].starts_with("| build | 0 | uses | uses: actions/checkout@v4 |"));
    // Stored text is literal: a pipe and a newline cannot break the table.
    assert!(steps[1].starts_with(
        "| build | 1 | run | name: Compile; run: npm ci \\| tee log ⏎ npm run build |"
    ));
    assert!(steps[1].contains(&format!("{SPAN_PREFIX} 320..340 @ 0123abc")));
    assert!(
        steps[2]
            .starts_with("| deploy | 0 | run | run: ./deploy.sh; secret names: DEPLOY\\_TOKEN |")
    );

    // A Dockerfile: stages are jobs, ordered by what each builds on.
    let stages = table(content, "acme/shop@Dockerfile", "Jobs in dependency order");
    assert!(
        stages[0].starts_with("| 1 | stage-0 | stage | 0 | — | name: build; base image: node:20 |")
    );
    assert!(stages[1].starts_with("| 2 | stage-1 | stage | 1 | stage-0 (via copy-from) |"));
    assert_eq!(
        table(content, "acme/shop@Dockerfile", "Triggers"),
        ["| — | None recovered | — | — | — | — | — |"]
    );
}

// AC-0650 (R-INT-4): a job with an unresolved need, a job that needs it
// and jobs on a cycle are never given a place; each is listed with the
// reason, after the jobs that are ordered.
#[test]
fn unresolved_need_and_cycle_are_stated_and_never_ordered() {
    let mut f = Fixture::default();
    f.pipeline(WORKFLOW, json!({"format": "github-actions"}), (0, 12));
    for (index, key) in ["a", "b", "c", "d", "e", "free"].into_iter().enumerate() {
        let start = 100 + index as u64 * 10;
        f.job(
            WORKFLOW,
            key,
            index as u64,
            json!({"kind": "job"}),
            (start, start + 9),
        );
    }
    f.dangling(WORKFLOW, "a", "missing", (101, 108));
    f.needs(WORKFLOW, "b", "a", "needs", (111, 112));
    f.needs(WORKFLOW, "c", "d", "needs", (121, 122));
    f.needs(WORKFLOW, "d", "c", "needs", (131, 132));
    f.needs(WORKFLOW, "e", "d", "needs", (141, 142));

    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let artifact = f.artifact(mode);
        let content = &artifact.content;
        assert!(content.contains("Jobs: 6, of which not ordered: 5."));
        assert!(content.contains("5 of 6 jobs are not ordered; each row says why."));
        let jobs = table(content, WORKFLOW_SECTION, "Jobs in dependency order");
        let order: Vec<(&str, &str)> = jobs
            .iter()
            .map(|row| {
                let mut cells = row.split(" | ");
                (
                    cells.next().unwrap().trim_start_matches("| "),
                    cells.next().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            order,
            [
                ("1", "free"),
                ("not ordered: a need is not resolved", "a"),
                ("not ordered: needs a job that is not ordered", "b"),
                (
                    "not ordered: its needs form a cycle, or it needs a job on one",
                    "c"
                ),
                (
                    "not ordered: its needs form a cycle, or it needs a job on one",
                    "d"
                ),
                (
                    "not ordered: its needs form a cycle, or it needs a job on one",
                    "e"
                ),
            ]
        );
        assert!(jobs[1].contains(
            "| not resolved: missing (\\`needs\\` names a job this workflow does not declare) |"
        ));
        // The unresolved need is cited as the Gap it is.
        let gap = artifact
            .assertions
            .iter()
            .find(|assertion| assertion.subject_id.contains(" NEEDS gap:pipeline:"))
            .expect("the dangling need is asserted");
        assert_eq!(gap.provenance.confidence_tier, ConfidenceTier::Gap);
    }

    // A need whose job is not among the admitted facts is not resolved
    // either, and a self-need is a cycle.
    let mut f = Fixture::default();
    f.pipeline(WORKFLOW, json!({"format": "github-actions"}), (0, 12));
    f.job(WORKFLOW, "a", 0, json!({"kind": "job"}), (100, 109));
    f.job(WORKFLOW, "b", 1, json!({"kind": "job"}), (110, 119));
    f.needs(WORKFLOW, "a", "absent", "needs", (101, 102));
    f.needs(WORKFLOW, "b", "b", "needs", (111, 112));
    let content = f.artifact(ExportMode::VerifiedOnly).content;
    let jobs = table(&content, WORKFLOW_SECTION, "Jobs in dependency order");
    assert!(jobs[0].starts_with(
        "| not ordered: a need is not resolved | a | job | 0 | not resolved: absent (the \
         needed job is not among the facts this export admits) |"
    ));
    assert!(
        jobs[1]
            .starts_with("| not ordered: its needs form a cycle, or it needs a job on one | b |")
    );
}

// AC-0650 (R-INT-5): a verified-only export leaves out InferredWeak rows
// and states how many, cites none of them, and says which admitted facts
// lost their place; a best-effort export lists them marked.
#[test]
fn verified_only_leaves_out_weak_rows_and_counts_them_and_best_effort_marks_them() {
    let mut f = deploy();
    // A weak step under a confirmed job, and a weak job with a confirmed step.
    let weak_step = step_id(WORKFLOW, "build", 1);
    f.fact(&weak_step).props["prov"] = weak_prov(WORKFLOW, (320, 340), &weak_step);
    let weak_job = job_id(WORKFLOW, "lint");
    f.fact(&weak_job).props["prov"] = weak_prov(WORKFLOW, (400, 440), &weak_job);
    f.step(
        WORKFLOW,
        "lint",
        0,
        json!({"kind": "run", "run": "npm run lint"}),
        (420, 440),
    );

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
        "Pipeline facts left out of this export because they are InferredWeak or rejected: 2."
    ));
    assert!(content.contains(
        "1 admitted trigger, job or step facts are not listed: the pipeline, the job or the \
         placing relation above each is not among the facts this export admits."
    ));
    assert!(content.contains("Jobs: 5, of which not ordered: 0. Steps: 3."));
    assert!(!content.contains("npm run lint") && !content.contains("Compile"));
    assert!(!content.contains("InferredWeak fact"));
    assert!(verified.assertions.iter().all(|assertion| {
        assertion.provenance.confidence_tier != ConfidenceTier::InferredWeak
            && !assertion.subject_id.contains("#lint")
            && !assertion.subject_id.contains(&weak_step)
    }));

    let best = f.artifact(ExportMode::BestEffort);
    let content = &best.content;
    assert!(content.contains(
        "InferredWeak rows are included and marked in the Note column. Pipeline facts left \
         out of this export because they are rejected: 0."
    ));
    assert!(!content.contains("are not listed"));
    const MARK: &str = "| rests on an InferredWeak fact; a verified-only export omits this row |";
    let jobs = table(content, WORKFLOW_SECTION, "Jobs in dependency order");
    let lint = jobs.iter().find(|row| row.contains("| lint |")).unwrap();
    assert!(lint.contains("| Agentic | InferredWeak |") && lint.ends_with(MARK));
    let steps = table(content, WORKFLOW_SECTION, "Steps in declared order");
    assert!(steps[1].contains("name: Compile") && steps[1].ends_with(MARK));
    // A confirmed step under the weak job is listed and is not itself marked.
    let under = steps
        .iter()
        .find(|row| row.contains("npm run lint"))
        .unwrap();
    assert!(under.ends_with("| — |"));
    assert_eq!(content.matches(MARK).count(), 2);

    // A weak need is marked where it is declared; verified-only drops it and
    // the job is then ordered without it.
    let mut f = deploy();
    let need = f
        .edges
        .iter_mut()
        .find(|edge| edge.label == "NEEDS" && edge.src == job_id(WORKFLOW, "test"))
        .unwrap();
    need.props["prov"] = weak_prov(WORKFLOW, (210, 215), "weak need");
    let best = f.artifact(ExportMode::BestEffort).content;
    assert!(best.contains(
        "| build (via needs) [InferredWeak; a verified-only export leaves this need out] |"
    ));
    let verified = f.artifact(ExportMode::VerifiedOnly).content;
    assert!(verified.contains("InferredWeak or rejected: 1."));
    let jobs = table(&verified, WORKFLOW_SECTION, "Jobs in dependency order");
    assert!(jobs[0].starts_with("| 1 | test | job | 1 | — |"));

    // A rejected InferredStrong fact is withheld and counted in both modes.
    let mut f = deploy();
    let strong = prov(
        Tier::Semantic,
        ConfidenceTier::InferredStrong,
        WORKFLOW,
        (400, 440),
        "strong job",
    );
    let hash = strong["content_hash"].as_str().unwrap().to_owned();
    f.fact(&weak_job).props["prov"] = strong;
    let rejected = find(compile_spec(
        &f.nodes,
        &f.edges,
        &[],
        ExportMode::BestEffort,
        &BTreeSet::from([hash]),
    ));
    assert!(rejected.content.contains("because they are rejected: 1."));
    assert!(!rejected.content.contains("| lint |"));
}

fn stored(repo: &str, inventory: Value) -> StoredCoverage {
    let record = CoverageRecord {
        repo_key: repo.into(),
        registered_source_id: None,
        ingest_id: "job:7".into(),
        config_hash: "config".into(),
        inventory_json: inventory.to_string(),
    };
    StoredCoverage {
        inventory_hash: record.inventory_hash(),
        record,
    }
}

/// The `unsupported` list as `ingest::coverage::RepoCoverage` serializes it.
fn inventory() -> Value {
    json!({
        "inventory": "coverage@1",
        "unsupported": [
            {"capability": "Ruby", "occurrences": 3, "first_site": "legacy/report.rb:1",
             "reason": "no installed producer covers Ruby: a tool limitation, not a system gap"},
            {"capability": "unreadable-pipeline-file", "occurrences": 2,
             "first_site": ".github/workflows/aliased.yml:1",
             "reason": "GitHub Actions workflow is present and its steps were not recovered: \
                        workflow uses a YAML alias, anchor or tag: a tool limitation, not a \
                        system gap"},
            {"capability": "gitlab-ci", "occurrences": 1, "first_site": ".gitlab-ci.yml:1",
             "reason": "GitLab CI configuration is present and not read: no installed producer \
                        recovers its pipeline order: a tool limitation, not a system gap"}
        ],
    })
}

// AC-0650: files present and not read are listed with their recorded
// reason from the stored coverage inventory, identically in both modes;
// without an inventory the document says it states nothing about them.
#[test]
fn files_present_and_not_read_come_from_the_stored_inventory() {
    let f = deploy();
    let coverage = [
        stored("zeta/unreadable", json!({"producers": []})),
        stored(REPO, inventory()),
    ];
    let compile = |mode| {
        find(compile_spec_with_coverage(
            &f.nodes,
            &f.edges,
            &[],
            mode,
            &BTreeSet::new(),
            &coverage,
        ))
    };
    let section = |content: &str| {
        let start = content.find("\n## Files present and not read\n").unwrap();
        let end = content.find("\n## Coverage note").unwrap();
        content[start..end].to_owned()
    };
    let verified = compile(ExportMode::VerifiedOnly);
    let text = section(&verified.content);
    assert_eq!(text, section(&compile(ExportMode::BestEffort).content));
    let hash = &coverage[1].inventory_hash;
    // In the reader's order of kinds, not the inventory's.
    let rows: Vec<&str> = text.lines().filter(|l| l.starts_with("| acme")).collect();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].starts_with(
        "| acme/shop | GitLab CI configuration | 1 | .gitlab-ci.yml:1 | GitLab CI \
         configuration is present and not read:"
    ));
    assert!(rows[1].starts_with(
        "| acme/shop | GitHub Actions workflow or Dockerfile that could not be read exactly | \
         2 | .github/workflows/aliased.yml:1 | GitHub Actions workflow is present and its steps \
         were not recovered: workflow uses a YAML alias, anchor or tag:"
    ));
    assert!(rows.iter().all(|row| row.ends_with(&format!("| {hash} |"))));
    assert!(!text.contains("Ruby"));
    assert!(text.contains(
        "Stored inventories read: 1. Inventories this version cannot read, about which nothing \
         is stated: 1."
    ));
    // Inventory rows are not graph facts: they add no assertion.
    assert_eq!(
        verified.assertions,
        f.artifact(ExportMode::VerifiedOnly).assertions
    );

    let none = section(&f.artifact(ExportMode::VerifiedOnly).content);
    assert!(none.contains("No stored coverage inventory was supplied to this compilation"));
    assert!(!none.contains("None recorded"));

    let clean = [stored(REPO, json!({"unsupported": []}))];
    let clean = find(compile_spec_with_coverage(
        &f.nodes,
        &f.edges,
        &[],
        ExportMode::VerifiedOnly,
        &BTreeSet::new(),
        &clean,
    ));
    assert!(section(&clean.content).contains("| — | None recorded | 0 | — | — | — |"));
}

// AC-0650: the reader gaps that remain are stated in every export, and
// an export with no pipeline fact says so instead of rendering nothing.
#[test]
fn coverage_note_lists_what_the_reader_does_not_cover() {
    for fixture in [deploy(), Fixture::default()] {
        let content = fixture.artifact(ExportMode::VerifiedOnly).content;
        let note = &content[content
            .find("\n## Coverage note: what the pipeline reader does not cover\n")
            .unwrap()..];
        for stated in [
            "A Dockerfile that uses a heredoc (`<<`) or changes its escape character is not read",
            "A workflow that uses a YAML alias, anchor or tag is not read",
            "`Containerfile`",
            "GitLab CI, Jenkins, CircleCI and Azure Pipelines files are named as present and \
             are not read",
            "A matrix is one job with its matrix as written",
        ] {
            assert!(note.contains(stated), "{stated}");
        }
    }
    let empty = Fixture::default().artifact(ExportMode::BestEffort);
    assert!(
        empty
            .content
            .contains("| — | — | None in this export | 0 | 0 | 0 |")
    );
    assert!(empty.assertions.is_empty());
}

// AC-0650: the same graph yields the same bytes whatever order the store
// or the caller hands the facts and inventories over in, and the selected
// compiler agrees.
#[test]
fn two_exports_are_byte_identical_whatever_the_input_order() {
    let mut f = deploy();
    f.dangling(WORKFLOW, "lint", "missing", (401, 408));
    let coverage = [stored(REPO, inventory()), stored("acme/other", inventory())];
    let reversed_coverage: Vec<StoredCoverage> = coverage.iter().rev().cloned().collect();
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let compile = |f: &Fixture, coverage: &[StoredCoverage]| {
            find(compile_spec_with_coverage(
                &f.nodes,
                &f.edges,
                &[],
                mode,
                &BTreeSet::new(),
                coverage,
            ))
        };
        let first = compile(&f, &coverage);
        assert_eq!(compile(&f, &coverage), first);
        let reversed = Fixture {
            nodes: f.nodes.iter().rev().cloned().collect(),
            edges: f.edges.iter().rev().cloned().collect(),
        };
        assert_eq!(compile(&reversed, &reversed_coverage), first);
        assert!(first.verify_identity());

        let selected = compile_spec_selected_with_coverage(
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
            &reversed_coverage,
        )
        .unwrap();
        assert_eq!(selected, [SelectedArtifactOutcome::Produced(first)]);
    }
}

// AC-0650 (ADR-0058): every assertion of the artifact has a row in the
// export's fact index that names this artifact, the ids it cites pass the
// export's citation check, and an id that does not resolve, or two facts
// sharing one id, fail the export instead of producing a document.
#[test]
fn every_row_cites_a_fact_that_resolves_in_the_fact_index() {
    let mut f = deploy();
    f.dangling(WORKFLOW, "lint", "missing", (401, 408));
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
        // 2 pipelines, 2 triggers, 6 jobs and 4 steps with their placing
        // relations, and 5 needs.
        assert_eq!(artifact.assertions.len(), 2 + 2 * 2 + 6 * 2 + 4 * 2 + 5);
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
            id: step_id(WORKFLOW, "build", 9),
        };
        assert_eq!(
            check_citations(&index, [citation_id(&unknown).as_str()]),
            Err(FactIndexError::Unresolved {
                citation_id: citation_id(&unknown)
            })
        );
    }

    // The compile path refuses an export whose pipeline citations collide.
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
    .expect_err("no bundle, and so no pipelines.md, for a collision");
    assert!(matches!(error, FactIndexError::Collision { .. }));
}
