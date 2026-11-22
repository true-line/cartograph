use super::*;

const REPO: &str = "local/app";

const WORKFLOW: &str = r#"name: Deploy
on:
  push:
    branches: [main]
  workflow_dispatch:
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Compile
        run: npm ci && npm run build
  test:
    needs: build
    runs-on: ubuntu-latest
    strategy:
      matrix:
        node: [20, 22]
    steps:
      - uses: actions/checkout@v4
      - run: |
          npm ci
          npm test
  deploy:
    needs: [build, test]
    runs-on: ubuntu-latest
    environment: production
    steps:
      - name: Publish
        run: ./deploy.sh --token "${{ secrets.DEPLOY_TOKEN }}"
        env:
          REGISTRY_PASSWORD: ${{ secrets['REGISTRY_PASSWORD'] }}
"#;

const DOCKERFILE: &str = "ARG NODE_VERSION=20\n\
FROM node:${NODE_VERSION} AS build\n\
WORKDIR /src\n\
COPY . .\n\
RUN npm ci \\\n    && npm run build\n\
\n\
# runtime image\n\
FROM nginx:1.27\n\
COPY --from=build /src/dist /usr/share/nginx/html\n\
EXPOSE 80\n";

const GITLAB: &str = "stages: [build]\nbuild:\n  script: make\n";

fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, body) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
}

fn extract(files: &[(&str, &str)]) -> PipelineExtraction {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), files);
    extract_dir(
        dir.path(),
        &SourceId {
            repo: REPO,
            commit: "abc",
        },
    )
    .unwrap()
}

fn node<'a>(out: &'a PipelineExtraction, id: &str) -> &'a Node {
    out.nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| panic!("no node {id}"))
}

fn edge<'a>(out: &'a PipelineExtraction, src: &str, label: &str, dst: &str) -> &'a Edge {
    out.edges
        .iter()
        .find(|edge| edge.src == src && edge.label == label && edge.dst == dst)
        .unwrap_or_else(|| panic!("no edge {src} {label} {dst}"))
}

/// The source text a fact's first evidence span cites.
fn cited<'a>(props: &serde_json::Value, text: &'a str) -> &'a str {
    let span = &props["prov"]["evidence"][0];
    &text
        [span["byte_start"].as_u64().unwrap() as usize..span["byte_end"].as_u64().unwrap() as usize]
}

/// Nodes of `label` in `file`, ordered by their stored `index`.
fn ordered<'a>(out: &'a PipelineExtraction, label: &str, file: &str) -> Vec<&'a Node> {
    let mut nodes: Vec<&Node> = out
        .nodes
        .iter()
        .filter(|node| node.label == label && node.props["file"] == file)
        .collect();
    nodes.sort_by_key(|node| {
        (
            node.id.rsplit_once('/').map(|(job, _)| job.to_string()),
            node.props["index"].as_u64().unwrap(),
        )
    });
    nodes
}

const WF: &str = ".github/workflows/deploy.yml";

fn fixture() -> PipelineExtraction {
    extract(&[
        (WF, WORKFLOW),
        ("Dockerfile", DOCKERFILE),
        (".gitlab-ci.yml", GITLAB),
    ])
}

// AC-0598: a workflow with three jobs and a `needs` chain yields its
// triggers, jobs and steps in declared and dependency order, every fact T0
// Confirmed and citing the exact span that declares it.
#[test]
fn workflow_jobs_and_steps_are_ordered_facts_with_spans() {
    let out = fixture();
    assert_eq!(out.files, 2);
    assert!(out.unread.is_empty());
    for (tier, confidence, extractor) in out
        .nodes
        .iter()
        .map(|node| &node.props)
        .chain(out.edges.iter().map(|edge| &edge.props))
        .map(|props| {
            (
                props["prov"]["tier"].clone(),
                props["prov"]["confidence_tier"].clone(),
                props["prov"]["extractor_id"].clone(),
            )
        })
    {
        assert_eq!(tier, "Deterministic");
        assert_eq!(confidence, "Confirmed");
        assert_eq!(extractor, PIPELINE_EXTRACTOR_ID);
    }

    let pipeline = node(&out, &format!("pipeline:{REPO}@{WF}"));
    assert_eq!(pipeline.label, "Pipeline");
    assert_eq!(pipeline.props["format"], "github-actions");
    assert_eq!(pipeline.props["name"], "Deploy");
    assert_eq!(cited(&pipeline.props, WORKFLOW), "name: Deploy");

    // Triggers, in declared order, with their filters as declared.
    let triggers = ordered(&out, "PipelineTrigger", WF);
    let events: Vec<_> = triggers.iter().map(|t| t.props["event"].clone()).collect();
    assert_eq!(events, ["push", "workflow_dispatch"]);
    assert_eq!(triggers[0].props["config"], "branches: [main]");
    assert_eq!(
        cited(&triggers[0].props, WORKFLOW),
        "push:\n    branches: [main]"
    );
    assert_eq!(cited(&triggers[1].props, WORKFLOW), "workflow_dispatch:");

    // Jobs: declared order is `index`, dependency order is `depth` and the
    // NEEDS edges.
    let jobs = ordered(&out, "PipelineJob", WF);
    let summary: Vec<_> = jobs
        .iter()
        .map(|job| {
            (
                job.props["id"].as_str().unwrap(),
                job.props["index"].as_u64().unwrap(),
                job.props["depth"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(summary, [("build", 0, 0), ("test", 1, 1), ("deploy", 2, 2)]);
    let job = |id: &str| format!("pipeline-job:{REPO}@{WF}#{id}");
    assert!(cited(&jobs[1].props, WORKFLOW).starts_with("test:\n    needs: build"));
    assert!(cited(&jobs[1].props, WORKFLOW).ends_with("npm test"));
    let needs: Vec<(&str, &str)> = out
        .edges
        .iter()
        .filter(|edge| edge.label == "NEEDS" && edge.src.contains(WF))
        .map(|edge| (edge.src.as_str(), edge.dst.as_str()))
        .collect();
    assert_eq!(
        needs,
        [
            (job("test").as_str(), job("build").as_str()),
            (job("deploy").as_str(), job("build").as_str()),
            (job("deploy").as_str(), job("test").as_str()),
        ]
    );
    let chain = edge(&out, &job("deploy"), "NEEDS", &job("test"));
    assert_eq!(chain.props["via"], serde_json::json!(["needs"]));
    assert_eq!(cited(&chain.props, WORKFLOW), "test");
    assert_eq!(
        edge(
            &out,
            &job("deploy"),
            "PART_OF",
            &format!("pipeline:{REPO}@{WF}")
        )
        .props["index"],
        2
    );
    assert_eq!(jobs[2].props["environment"], "production");

    // Steps: declared order within each job, with `uses` or `run`.
    let steps: Vec<_> = ordered(&out, "PipelineStep", WF)
        .into_iter()
        .map(|step| {
            (
                step.id.rsplit_once('#').unwrap().1,
                step.props["kind"].as_str().unwrap(),
                step.props["uses"]
                    .as_str()
                    .or(step.props["run"].as_str())
                    .unwrap(),
            )
        })
        .collect();
    assert_eq!(
        steps,
        [
            ("build/0", "uses", "actions/checkout@v4"),
            ("build/1", "run", "npm ci && npm run build"),
            (
                "deploy/0",
                "run",
                "./deploy.sh --token \"${{ secrets.DEPLOY_TOKEN }}\""
            ),
            ("test/0", "uses", "actions/checkout@v4"),
            ("test/1", "run", "npm ci\nnpm test"),
        ]
    );
    let compile = node(&out, &format!("pipeline-step:{REPO}@{WF}#build/1"));
    assert_eq!(compile.props["name"], "Compile");
    assert_eq!(
        cited(&compile.props, WORKFLOW),
        "name: Compile\n        run: npm ci && npm run build"
    );
    let step_edge = edge(&out, &compile.id, "PART_OF", &job("build"));
    assert_eq!(step_edge.props["index"], 1);
    let block = node(&out, &format!("pipeline-step:{REPO}@{WF}#test/1"));
    assert_eq!(block.props["run_style"], "literal");

    // A matrix is one job with its matrix as declared, never expanded.
    assert_eq!(jobs[1].props["matrix"], "node: [20, 22]");
    assert_eq!(jobs.len(), 3);
}

// AC-0598: a two-stage Dockerfile yields its stages, base images, ordered
// instructions, the final stage and the `COPY --from` relation, with spans,
// through the logical-instruction parser the schedule reader uses.
#[test]
fn dockerfile_stages_and_instructions_are_ordered_facts_with_spans() {
    let out = fixture();
    let pipeline = node(&out, &format!("pipeline:{REPO}@Dockerfile"));
    assert_eq!(pipeline.props["format"], "dockerfile");
    assert_eq!(pipeline.props["final_stage"], "stage-1");
    assert_eq!(pipeline.props["stages"], 2);
    assert_eq!(
        pipeline.props["global_args"],
        serde_json::json!(["NODE_VERSION"])
    );

    let stages = ordered(&out, "PipelineJob", "Dockerfile");
    let summary: Vec<_> = stages
        .iter()
        .map(|stage| {
            (
                stage.props["kind"].as_str().unwrap(),
                stage.props["index"].as_u64().unwrap(),
                stage.props["base_image"].as_str().unwrap(),
                stage.props["name"].as_str(),
                stage.props["final"].as_bool(),
                stage.props["depth"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            // The base image is recorded as declared: `${…}` is not evaluated.
            ("stage", 0, "node:${NODE_VERSION}", Some("build"), None, 0),
            ("stage", 1, "nginx:1.27", None, Some(true), 1),
        ]
    );
    assert_eq!(
        cited(&stages[0].props, DOCKERFILE),
        "FROM node:${NODE_VERSION} AS build"
    );

    let stage = |index: usize| format!("pipeline-job:{REPO}@Dockerfile#stage-{index}");
    let copy = edge(&out, &stage(1), "NEEDS", &stage(0));
    assert_eq!(copy.props["via"], serde_json::json!(["copy-from"]));
    assert_eq!(
        cited(&copy.props, DOCKERFILE),
        "COPY --from=build /src/dist /usr/share/nginx/html"
    );

    let instructions: Vec<_> = ordered(&out, "PipelineStep", "Dockerfile")
        .into_iter()
        .map(|step| {
            (
                step.id.rsplit_once('#').unwrap().1,
                step.props["instruction"].as_str().unwrap(),
                step.props["text"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        instructions,
        [
            ("stage-0/0", "FROM", "FROM node:${NODE_VERSION} AS build"),
            ("stage-0/1", "WORKDIR", "WORKDIR /src"),
            ("stage-0/2", "COPY", "COPY . ."),
            ("stage-0/3", "RUN", "RUN npm ci  && npm run build"),
            ("stage-1/0", "FROM", "FROM nginx:1.27"),
            (
                "stage-1/1",
                "COPY",
                "COPY --from=build /src/dist /usr/share/nginx/html"
            ),
            ("stage-1/2", "EXPOSE", "EXPOSE 80"),
        ]
    );
    // A continued instruction is one fact spanning all of its lines.
    let run = node(&out, &format!("pipeline-step:{REPO}@Dockerfile#stage-0/3"));
    assert_eq!(
        cited(&run.props, DOCKERFILE),
        "RUN npm ci \\\n    && npm run build"
    );
    let copied = node(&out, &format!("pipeline-step:{REPO}@Dockerfile#stage-1/1"));
    assert_eq!(copied.props["copy_from"], "build");
    assert_eq!(copied.props["copy_from_stage"], "stage-0");
}

// AC-0598: another CI system's file beside them is present and not read: no
// fact is emitted for it, and the coverage inventory names it with its
// occurrence count and first site.
#[test]
fn other_ci_systems_are_listed_in_coverage_as_present_and_not_read() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            (WF, WORKFLOW),
            ("Dockerfile", DOCKERFILE),
            (".gitlab-ci.yml", GITLAB),
            ("services/api/.gitlab-ci.yml", GITLAB),
            ("Jenkinsfile", "pipeline { agent any }\n"),
            (".circleci/config.yml", "version: 2.1\n"),
            ("azure-pipelines.yml", "trigger: [main]\n"),
        ],
    );
    let out = extract_dir(
        dir.path(),
        &SourceId {
            repo: REPO,
            commit: "abc",
        },
    )
    .unwrap();
    assert!(out.nodes.iter().all(|node| {
        let file = node.props["file"].as_str().unwrap();
        file == WF || file == "Dockerfile"
    }));

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
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("circleci", 1, ".circleci/config.yml:1"),
            ("gitlab-ci", 2, ".gitlab-ci.yml:1"),
            ("jenkins-pipeline", 1, "Jenkinsfile:1"),
            ("azure-pipelines", 1, "azure-pipelines.yml:1"),
        ]
    );
    let gitlab = coverage
        .unsupported
        .iter()
        .find(|capability| capability.capability == "gitlab-ci")
        .unwrap();
    assert_eq!(
        gitlab.reason,
        "GitLab CI configuration is present and not read: no installed producer recovers \
         its pipeline order: a tool limitation, not a system gap"
    );
    assert!(coverage.limited);
    // The readable workflow and Dockerfile carry no limit.
    assert!(
        coverage
            .unsupported
            .iter()
            .all(|capability| capability.capability != UNREADABLE_KIND)
    );
}

// AC-0598: a secret reference is recorded by name; command text is redacted
// before it is stored, and `ENV` / `ARG` keep names only.
#[test]
fn secret_references_store_the_name_only_and_command_text_is_redacted() {
    let token = "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v2";
    let workflow = format!(
        r#"on: push
jobs:
  release:
    runs-on: ubuntu-latest
    if: secrets.GATE != ''
    steps:
      - run: 'curl -H "Authorization: Bearer abcdefgh12345678" https://example.test'
      - run: ./publish --key {token}
        env:
          NPM_TOKEN: ${{{{ secrets.NPM_TOKEN }}}}
      - uses: acme/deploy@0123456789abcdef0123456789abcdef01234567
        with:
          api-key: ${{{{ secrets.API_KEY }}}}
          literal: {token}
"#
    );
    let dockerfile = format!(
        "FROM alpine:3.20\nENV API_TOKEN={token} MODE=\"a b\"\nARG BUILD_KEY={token}\n\
         RUN echo password=hunter2hunter2 && fetch {token}\n"
    );
    let wf = ".github/workflows/release.yml";
    let out = extract(&[(wf, &workflow), ("Dockerfile", &dockerfile)]);
    assert!(out.unread.is_empty());

    // Names only, on the workflow, the job and the step that reference them.
    let pipeline = node(&out, &format!("pipeline:{REPO}@{wf}"));
    assert_eq!(
        pipeline.props["secrets"],
        serde_json::json!(["API_KEY", "NPM_TOKEN"])
    );
    let job = node(&out, &format!("pipeline-job:{REPO}@{wf}#release"));
    assert_eq!(
        job.props["secrets"],
        serde_json::json!(["API_KEY", "GATE", "NPM_TOKEN"])
    );
    let step = |index: usize| node(&out, &format!("pipeline-step:{REPO}@{wf}#release/{index}"));
    assert_eq!(step(1).props["secrets"], serde_json::json!(["NPM_TOKEN"]));
    assert_eq!(step(2).props["secrets"], serde_json::json!(["API_KEY"]));
    // `with` contributes input names, never values.
    assert_eq!(
        step(2).props["with_keys"],
        serde_json::json!(["api-key", "literal"])
    );

    // Command text: recognized secrets replaced, token-shaped runs withheld,
    // and the count disclosed.
    assert_eq!(
        step(0).props["run"],
        "curl -H \"Authorization: Bearer [REDACTED]\" https://example.test"
    );
    assert_eq!(step(0).props["redactions"], 1);
    assert_eq!(step(1).props["run"], "./publish --key [WITHHELD]");
    assert_eq!(step(1).props["redactions"], 1);
    // An action reference keeps its pinned commit.
    assert_eq!(
        step(2).props["uses"],
        "acme/deploy@0123456789abcdef0123456789abcdef01234567"
    );

    let instruction = |index: usize| {
        node(
            &out,
            &format!("pipeline-step:{REPO}@Dockerfile#stage-0/{index}"),
        )
    };
    assert_eq!(instruction(1).props["text"], "ENV API_TOKEN MODE");
    assert_eq!(
        instruction(1).props["names"],
        serde_json::json!(["API_TOKEN", "MODE"])
    );
    assert_eq!(instruction(1).props["values_withheld"], true);
    assert_eq!(instruction(2).props["text"], "ARG BUILD_KEY");
    assert_eq!(
        instruction(3).props["text"],
        "RUN echo password=[REDACTED] && fetch [WITHHELD]"
    );
    assert_eq!(instruction(3).props["redactions"], 2);

    // No stored property, on any fact, carries a value.
    let stored = serde_json::to_string(&(&out.nodes, &out.edges)).unwrap();
    for value in [token, "abcdefgh12345678", "hunter2hunter2"] {
        assert!(!stored.contains(value), "{value} was stored");
    }
}

// AC-0598: an unparseable or partial file yields no facts at all, never a
// partial order, and the limit is recorded for the coverage inventory.
#[test]
fn malformed_workflow_yields_no_step_facts_and_a_recorded_limit() {
    let broken = "on: push\njobs:\n  build:\n    steps:\n      - run: make\n   bad: [unclosed\n";
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            (".github/workflows/broken.yml", broken),
            // Valid YAML that is not a readable workflow.
            (
                ".github/workflows/stepless.yml",
                "on: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n",
            ),
            (
                ".github/workflows/aliased.yml",
                "on: push\njobs:\n  a: &job\n    steps:\n      - run: make\n  b: *job\n",
            ),
            (
                ".github/workflows/cyclic.yml",
                "on: push\njobs:\n  a:\n    needs: b\n    steps:\n      - run: make\n  b:\n    needs: a\n    steps:\n      - run: make\n",
            ),
            ("Dockerfile", "FROM alpine\nRUN cat <<EOF > /x\nhi\nEOF\n"),
            (
                ".github/workflows/ok.yml",
                "on: push\njobs:\n  a:\n    steps:\n      - run: make\n",
            ),
        ],
    );
    let out = extract_dir(
        dir.path(),
        &SourceId {
            repo: REPO,
            commit: "abc",
        },
    )
    .unwrap();
    assert_eq!(out.files, 1);
    assert!(
        out.nodes
            .iter()
            .all(|node| node.props["file"] == ".github/workflows/ok.yml")
    );
    let unread: Vec<_> = out
        .unread
        .iter()
        .map(|unread| (unread.path.as_str(), unread.reason))
        .collect();
    assert_eq!(
        unread,
        [
            (
                ".github/workflows/aliased.yml",
                "workflow uses a YAML alias, anchor or tag"
            ),
            (".github/workflows/broken.yml", "file is not valid YAML"),
            (".github/workflows/cyclic.yml", "job needs form a cycle"),
            (
                ".github/workflows/stepless.yml",
                "a job declares neither `steps` nor `uses`"
            ),
            ("Dockerfile", "Dockerfile uses a heredoc"),
        ]
    );

    let report = crate::preflight::preflight(dir.path()).unwrap();
    let coverage = crate::coverage::RepoCoverage::from_facts(&crate::coverage::CoverageFacts {
        repo: REPO,
        commit: "abc",
        ..Default::default()
    })
    .with_unsupported(&report);
    let limit = coverage
        .unsupported
        .iter()
        .find(|capability| capability.capability == UNREADABLE_KIND)
        .unwrap();
    assert_eq!(limit.occurrences, 5);
    assert_eq!(limit.first_site, ".github/workflows/aliased.yml:1");
    assert!(limit.reason.starts_with(
        "GitHub Actions workflow is present and its steps were not recovered: workflow uses \
         a YAML alias, anchor or tag"
    ));
}

// AC-0598: the same tree yields identical facts and ids, whatever order its
// files were written in.
#[test]
fn two_runs_yield_identical_facts() {
    let files = [
        (WF, WORKFLOW),
        ("Dockerfile", DOCKERFILE),
        ("docker/Dockerfile.worker", DOCKERFILE),
        (
            ".github/workflows/ci.yaml",
            "on: [push, pull_request]\njobs:\n  a:\n    steps:\n      - run: make\n",
        ),
    ];
    let mut reversed = files;
    reversed.reverse();
    let first = extract(&files);
    let second = extract(&reversed);
    assert_eq!(first.files, 4);
    assert_eq!(first.nodes, second.nodes);
    assert_eq!(first.edges, second.edges);
    let ids: BTreeSet<&str> = first.nodes.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(ids.len(), first.nodes.len(), "node ids are distinct");
    let keys: BTreeSet<_> = first
        .edges
        .iter()
        .map(|edge| (&edge.src, &edge.dst, &edge.label))
        .collect();
    assert_eq!(keys.len(), first.edges.len(), "edge keys are distinct");
}

// AC-0598: a `needs` naming a job the workflow does not declare is an
// explicit Gap (R-INT-4); the jobs above it carry no dependency depth.
#[test]
fn dangling_needs_is_an_explicit_gap() {
    let workflow = "on: push\njobs:\n  a:\n    needs: [ghost]\n    steps:\n      - run: make\n  b:\n    needs: a\n    steps:\n      - run: make\n";
    let wf = ".github/workflows/ci.yml";
    let out = extract(&[(wf, workflow)]);
    let job = |id: &str| format!("pipeline-job:{REPO}@{wf}#{id}");
    let gap_id = format!("gap:pipeline:{REPO}@{wf}#a/needs/ghost");
    let gap = node(&out, &gap_id);
    assert_eq!(gap.label, "Gap");
    assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");
    assert_eq!(gap.props["needs"], "ghost");
    assert_eq!(cited(&gap.props, workflow), "ghost");
    let dangling = edge(&out, &job("a"), "NEEDS", &gap_id);
    assert_eq!(dangling.props["prov"]["confidence_tier"], "Gap");
    // The declared list is kept; the resolved relation is not invented.
    assert_eq!(
        node(&out, &job("a")).props["needs"],
        serde_json::json!(["ghost"])
    );
    assert!(node(&out, &job("a")).props.get("depth").is_none());
    assert!(node(&out, &job("b")).props.get("depth").is_none());
    assert_eq!(
        edge(&out, &job("b"), "NEEDS", &job("a")).props["prov"]["confidence_tier"],
        "Confirmed"
    );
}

// AC-0598: `on:` is read in all its forms; reusable workflows and
// expressions are references, never evaluated.
#[test]
fn trigger_forms_and_references_are_recorded_as_declared() {
    let events = |workflow: &str| -> Vec<String> {
        let out = extract(&[(".github/workflows/w.yml", workflow)]);
        assert!(out.unread.is_empty(), "{:?}", out.unread);
        ordered(&out, "PipelineTrigger", ".github/workflows/w.yml")
            .into_iter()
            .map(|trigger| trigger.props["event"].as_str().unwrap().to_string())
            .collect()
    };
    let jobs = "jobs:\n  a:\n    steps:\n      - run: make\n";
    assert_eq!(events(&format!("on: push\n{jobs}")), ["push"]);
    assert_eq!(
        events(&format!("on: [push, pull_request]\n{jobs}")),
        ["push", "pull_request"]
    );
    assert_eq!(
        events(&format!("on:\n  - release\n  - push\n{jobs}")),
        ["release", "push"]
    );
    assert_eq!(
        events(&format!(
            "on:\n  schedule:\n    - cron: '0 3 * * *'\n  push:\n{jobs}"
        )),
        ["schedule", "push"]
    );
    assert_eq!(
        events(&format!(
            "on: {{push: {{branches: [main]}}, workflow_dispatch}}\n{jobs}"
        )),
        ["push", "workflow_dispatch"]
    );

    let wf = ".github/workflows/call.yml";
    let out = extract(&[(
        wf,
        "on: push\njobs:\n  call:\n    if: ${{ github.ref == 'refs/heads/main' }}\n    uses: acme/platform/.github/workflows/deploy.yml@v2\n    secrets: inherit\n  local:\n    runs-on: ${{ matrix.os }}\n    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n    steps:\n      - uses: ./.github/actions/setup\n",
    )]);
    let call = node(&out, &format!("pipeline-job:{REPO}@{wf}#call"));
    assert_eq!(
        call.props["uses"],
        "acme/platform/.github/workflows/deploy.yml@v2"
    );
    assert_eq!(call.props["secrets_inherit"], true);
    assert_eq!(call.props["if"], "${{ github.ref == 'refs/heads/main' }}");
    let local = node(&out, &format!("pipeline-job:{REPO}@{wf}#local"));
    assert_eq!(local.props["runs_on"], "${{ matrix.os }}");
    assert_eq!(local.props["matrix"], "os: [ubuntu-latest, windows-latest]");
    // One job, not one per matrix cell; no step for the called workflow.
    assert_eq!(ordered(&out, "PipelineJob", wf).len(), 2);
    assert_eq!(ordered(&out, "PipelineStep", wf).len(), 1);
}

// AC-0598: a stage built on an earlier stage and a numeric `--from` are
// stage relations; an external `--from` image stays a declared reference.
#[test]
fn dockerfile_stage_relations_are_exact_or_left_as_declared() {
    let dockerfile = "FROM golang:1.23 AS Base\nRUN go version\nFROM base AS build\nRUN make\nFROM scratch\nCOPY --from=0 /a /a\nCOPY --from=BUILD /b /b\nCOPY --from=build /c /c\nCOPY --from=nginx:latest /etc/nginx /etc/nginx\n";
    let out = extract(&[("Dockerfile", dockerfile)]);
    let stage = |index: usize| format!("pipeline-job:{REPO}@Dockerfile#stage-{index}");
    let from = edge(&out, &stage(1), "NEEDS", &stage(0));
    assert_eq!(from.props["via"], serde_json::json!(["from"]));
    assert_eq!(node(&out, &stage(1)).props["base_stage"], "stage-0");
    assert_eq!(
        edge(&out, &stage(2), "NEEDS", &stage(0)).props["via"],
        serde_json::json!(["copy-from"])
    );
    // Two copies from one stage are one relation citing both instructions.
    let twice = edge(&out, &stage(2), "NEEDS", &stage(1));
    assert_eq!(twice.props["prov"]["evidence"].as_array().unwrap().len(), 2);
    assert_eq!(
        out.edges
            .iter()
            .filter(|edge| edge.label == "NEEDS")
            .count(),
        3
    );
    let external = node(&out, &format!("pipeline-step:{REPO}@Dockerfile#stage-2/4"));
    assert_eq!(external.props["copy_from"], "nginx:latest");
    assert!(external.props.get("copy_from_stage").is_none());
    assert_eq!(node(&out, &stage(2)).props["depth"], 2);
}

// AC-0650: `Containerfile`, a dotted suffix of it and any `*.dockerfile`
// name are read as Dockerfiles; an ignore list and a name that only contains
// the word are not. One the reader cannot read exactly is still a named
// limit under its own name.
#[test]
fn containerfile_and_dockerfile_suffix_names_are_read() {
    let stage = "FROM alpine:3.20\nRUN true\n";
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            ("Containerfile", stage),
            ("Containerfile.dev", stage),
            ("docker/api.dockerfile", stage),
            ("docker/Web.Dockerfile", stage),
            ("Containerfile.dockerignore", "node_modules\n"),
            (".dockerfile", stage),
            ("docs/api.dockerfile.md", stage),
            (
                "docker/heredoc.dockerfile",
                "FROM alpine\nRUN <<EOF\ntrue\nEOF\n",
            ),
        ],
    );
    let out = extract_dir(
        dir.path(),
        &SourceId {
            repo: REPO,
            commit: "abc",
        },
    )
    .unwrap();
    let mut pipelines: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Pipeline")
        .map(|node| {
            assert_eq!(node.props["format"], "dockerfile");
            node.props["file"].as_str().unwrap()
        })
        .collect();
    pipelines.sort_unstable();
    assert_eq!(
        pipelines,
        [
            "Containerfile",
            "Containerfile.dev",
            "docker/Web.Dockerfile",
            "docker/api.dockerfile",
        ]
    );
    assert_eq!(out.files, 4);
    assert_eq!(
        out.unread,
        [Unread {
            path: "docker/heredoc.dockerfile".into(),
            reason: "Dockerfile uses a heredoc",
        }]
    );
    let limit = unread(
        "heredoc.dockerfile",
        "docker/heredoc.dockerfile",
        &dir.path().join("docker/heredoc.dockerfile"),
    )
    .unwrap();
    assert_eq!(limit.kind, UNREADABLE_KIND);
    assert!(
        unread(
            "Containerfile",
            "Containerfile",
            &dir.path().join("Containerfile")
        )
        .is_none()
    );
}

// AC-0650: a `RUN --mount=…,from=` naming an earlier stage, by name or by
// index, is a stage relation citing the instruction; an external image stays
// a declared reference, and a quoted option list is not guessed at.
#[test]
fn run_mount_from_an_earlier_stage_is_a_stage_relation() {
    let dockerfile = "FROM golang:1.23 AS deps\nRUN go mod download\nFROM alpine AS tools\nRUN true\nFROM scratch\nRUN --mount=type=bind,FROM=Deps,source=/go/pkg,target=/go/pkg --mount=type=cache,target=/root/.cache --mount=from=1,target=/tools make\nRUN --mount=type=bind,from=busybox:1.36,target=/bb true\nCOPY --from=deps /a /a\n";
    let out = extract(&[("Dockerfile", dockerfile)]);
    let stage = |index: usize| format!("pipeline-job:{REPO}@Dockerfile#stage-{index}");
    // One relation per needed stage; both declarations of it are cited.
    let deps = edge(&out, &stage(2), "NEEDS", &stage(0));
    assert_eq!(
        deps.props["via"],
        serde_json::json!(["copy-from", "mount-from"])
    );
    assert_eq!(deps.props["prov"]["evidence"].as_array().unwrap().len(), 2);
    let tools = edge(&out, &stage(2), "NEEDS", &stage(1));
    assert_eq!(tools.props["via"], serde_json::json!(["mount-from"]));
    assert!(cited(&tools.props, dockerfile).starts_with("RUN --mount=type=bind,FROM=Deps"));
    assert_eq!(
        out.edges
            .iter()
            .filter(|edge| edge.label == "NEEDS")
            .count(),
        2
    );
    let run = node(&out, &format!("pipeline-step:{REPO}@Dockerfile#stage-2/1"));
    assert_eq!(run.props["mount_from"], serde_json::json!(["Deps", "1"]));
    assert_eq!(
        run.props["mount_from_stages"],
        serde_json::json!(["stage-0", "stage-1"])
    );
    let external = node(&out, &format!("pipeline-step:{REPO}@Dockerfile#stage-2/2"));
    assert_eq!(
        external.props["mount_from"],
        serde_json::json!(["busybox:1.36"])
    );
    assert!(external.props.get("mount_from_stages").is_none());
    assert_eq!(node(&out, &stage(2)).props["depth"], 1);

    let quoted = extract(&[(
        "Dockerfile",
        "FROM alpine AS a\nFROM alpine\nRUN --mount=type=bind,\"from=a\",target=/a true\n",
    )]);
    assert!(quoted.nodes.is_empty());
    assert_eq!(quoted.unread[0].reason, "a RUN --mount option is quoted");
}

// AC-1057: supplied bytes give exactly the facts the checkout walk gives for
// the same contents; the walk's selection is the pipeline files alone, and a
// supplied path this reader does not parse is ignored. (T-1057)
#[test]
fn supplied_sources_match_the_walk_and_replace_the_checkout() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            (WF, WORKFLOW),
            ("Dockerfile", DOCKERFILE),
            (".gitlab-ci.yml", GITLAB),
            ("src/app.ts", "export {};\n"),
        ],
    );
    let id = SourceId {
        repo: REPO,
        commit: "abc",
    };
    let paths = pipeline_paths(dir.path()).unwrap();
    assert_eq!(paths, [WF, "Dockerfile"]);
    let walked = extract_dir(dir.path(), &id).unwrap();
    // The checkout changes; the supplied bytes are the originals.
    write(dir.path(), &[("Dockerfile", "FROM scratch\n")]);
    let supplied = |rel: &str| match rel {
        WF => Ok(WORKFLOW.as_bytes().to_vec()),
        "Dockerfile" => Ok(DOCKERFILE.as_bytes().to_vec()),
        other => Err(std::io::Error::other(format!("not supplied: {other}"))),
    };
    let mut with_other = paths.clone();
    with_other.push("src/app.ts".into());
    let captured = extract_from_sources(&with_other, &supplied, &id);
    assert_eq!(captured.files, walked.files);
    assert_eq!(
        serde_json::to_string(&captured.nodes).unwrap(),
        serde_json::to_string(&walked.nodes).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&captured.edges).unwrap(),
        serde_json::to_string(&walked.edges).unwrap()
    );
    // Bytes that are not UTF-8 are unread, as on disk.
    let invalid = |_: &str| Ok(vec![0xff, 0xfe]);
    let unread = extract_from_sources(&paths, &invalid, &id);
    assert_eq!(unread.files, 0);
    assert_eq!(unread.unread.len(), 2);
}
