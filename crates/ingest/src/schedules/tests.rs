use super::*;

const REPO: &str = "local/app";

fn extract(files: &[(&str, &str)]) -> ScheduleExtraction {
    extract_with(files, &BTreeSet::new())
}

fn extract_with(files: &[(&str, &str)], symbols: &BTreeSet<String>) -> ScheduleExtraction {
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    extract_dir(
        dir.path(),
        &SourceId {
            repo: REPO,
            commit: "abc",
        },
        symbols,
    )
    .unwrap()
}

fn entry<'a>(out: &'a ScheduleExtraction, index: usize, file: &str) -> &'a Edge {
    let src = format!("sched:{REPO}@{file}#{index}");
    out.edges
        .iter()
        .find(|edge| edge.src == src && edge.label == "ENTRY")
        .unwrap()
}

fn confidence(edge: &Edge) -> &str {
    edge.props["prov"]["confidence_tier"].as_str().unwrap()
}

const DOCKERFILE: &str = "FROM node:20 AS npm\nWORKDIR /assets\nCOPY . .\n\nFROM python:3.12\nWORKDIR /code\nCOPY pyproject.toml ./\nCOPY . .\nCMD [\"gunicorn\"]\n";

const CRONTAB: &str = r#"jobs:
  - name: SimpleLogin growth stats
    command: python /code/cron.py -j stats
    shell: /bin/bash
    schedule: "0 0 * * *"
    captureStderr: true

  - name: Relative script
    command: python3.12 -u jobs/cleanup.py --all
    schedule:
      minute: 15
      hour: 1

  - name: Module job
    command: python -m app.tasks
    schedule: "*/5 * * * *"
"#;

// AC-0330: each yacron job is a Schedule trigger with its verbatim cron
// expression; an entry the final Dockerfile stage maps into the container
// is Confirmed and cites both files. (T-0330)
#[test]
fn yacron_jobs_become_schedules_entering_the_files_they_run() {
    let out = extract(&[
        ("Dockerfile", DOCKERFILE),
        ("crontab.yml", CRONTAB),
        ("cron.py", "import sys\n"),
        ("jobs/cleanup.py", ""),
        ("app/tasks/__main__.py", ""),
    ]);
    assert_eq!(out.files, 1);
    let schedules: Vec<&Node> = out.nodes.iter().filter(|n| n.label == "Schedule").collect();
    assert_eq!(schedules.len(), 3);
    assert_eq!(schedules[0].props["name"], "SimpleLogin growth stats");
    assert_eq!(schedules[0].props["schedule"], "0 0 * * *");
    assert_eq!(
        schedules[0].props["command"],
        "python /code/cron.py -j stats"
    );
    assert_eq!(schedules[1].props["schedule"], "minute: 15\n      hour: 1");

    let stats = entry(&out, 0, "crontab.yml");
    assert_eq!(stats.dst, format!("file:{REPO}@cron.py"));
    assert_eq!(confidence(stats), "Confirmed");
    assert_eq!(stats.props["container_dir"], "/code");
    let cited: Vec<&str> = stats.props["prov"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(cited, vec!["crontab.yml", "Dockerfile", "Dockerfile"]);

    let relative = entry(&out, 1, "crontab.yml");
    assert_eq!(relative.dst, format!("file:{REPO}@jobs/cleanup.py"));
    assert_eq!(confidence(relative), "Confirmed");
    let module = entry(&out, 2, "crontab.yml");
    assert_eq!(module.dst, format!("file:{REPO}@app/tasks/__main__.py"));
    // Entry files get a File node so the edge never dangles.
    assert!(
        out.nodes
            .iter()
            .any(|n| n.id == format!("file:{REPO}@cron.py"))
    );
}

// AC-0330: without a container mapping an entry is only InferredStrong and
// says why; a GitHub workflow's `jobs` mapping is not a schedule. (T-0330)
#[test]
fn unmapped_entries_are_inferred_and_marked() {
    let out = extract(&[
        (
            "ops/schedule.yaml",
            "jobs:\n  - name: a\n    command: python /srv/app/cron.py\n    schedule: \"1 * * * *\"\n  - name: b\n    command: bash scripts/run.sh\n    schedule: \"2 * * * *\"\n",
        ),
        ("cron.py", ""),
        ("scripts/run.sh", ""),
        (
            ".github/workflows/ci.yml",
            "on: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n",
        ),
    ]);
    assert_eq!(out.files, 1);
    let suffix = entry(&out, 0, "ops/schedule.yaml");
    assert_eq!(suffix.dst, format!("file:{REPO}@cron.py"));
    assert_eq!(confidence(suffix), "InferredStrong");
    assert_eq!(suffix.props["entry_path_suffix"], true);
    let relative = entry(&out, 1, "ops/schedule.yaml");
    assert_eq!(confidence(relative), "InferredStrong");
    assert_eq!(relative.props["entry_relative_to_repo_root"], true);
}

// AC-0330: what the repository can't prove is a Gap naming the job, never a
// guessed entry. (T-0330)
#[test]
fn unprovable_entries_are_gaps() {
    let jobs = [
        (
            "python /code/cron.py -j a && echo done",
            "shell-composed schedule command",
        ),
        (
            "python /code/missing.py",
            "schedule entry not found in source tree",
        ),
        (
            "python /opt/x/run.py",
            "schedule entry matches several files",
        ),
        ("gunicorn wsgi:app", "schedule command runs no script file"),
        ("python -u", "schedule command runs no script file"),
        ("node -e cron.py", "schedule command evaluates inline code"),
        (
            "node --require hook cron.py",
            "schedule command uses an unmodeled interpreter option",
        ),
    ];
    let mut crontab = String::from("jobs:\n");
    for (command, _) in &jobs {
        crontab.push_str(&format!(
            "  - command: {command}\n    schedule: \"* * * * *\"\n"
        ));
    }
    crontab.push_str("  - command: *shared\n    schedule: \"* * * * *\"\n");
    crontab.push_str("  - schedule: \"* * * * *\"\n");
    let out = extract(&[
        ("Dockerfile", DOCKERFILE),
        ("crontab.yml", &crontab),
        ("cron.py", ""),
        // Both are suffixes of /opt/x/run.py.
        ("x/run.py", ""),
        ("opt/x/run.py", ""),
    ]);
    let reasons: Vec<&str> = (0..jobs.len() + 2)
        .map(|index| {
            let edge = entry(&out, index, "crontab.yml");
            assert!(edge.dst.starts_with("gap:schedule:"), "{index}: {edge:?}");
            edge.props["reason"].as_str().unwrap()
        })
        .collect();
    let mut expected: Vec<&str> = jobs.iter().map(|(_, reason)| *reason).collect();
    expected.push("schedule job uses a YAML alias, anchor or tag");
    expected.push("schedule job without a literal command");
    assert_eq!(reasons, expected);
}

// AC-0330: only a single whole-repository copy in the final stage maps the
// container; earlier stages and a second copy prove nothing. (T-0330)
#[test]
fn container_root_comes_from_one_final_stage_copy() {
    assert_eq!(container_root(DOCKERFILE).unwrap().dir, "/code");
    assert_eq!(
        container_root("FROM a\nWORKDIR /srv\nWORKDIR app\nCOPY ./ ./\n")
            .unwrap()
            .dir,
        "/srv/app"
    );
    assert_eq!(
        container_root("FROM a\nCOPY . /opt/app/\n").unwrap().dir,
        "/opt/app"
    );
    for unproven in [
        "FROM a\nWORKDIR /code\nCOPY . .\nFROM b\nCMD x\n",
        "FROM a\nWORKDIR /code\nCOPY . .\nCOPY . /other\n",
        "FROM a\nCOPY . .\n",
        "FROM a\nWORKDIR /code\nCOPY --from=a . .\n",
        "FROM a\nWORKDIR /code\nCOPY src .\n",
    ] {
        assert_eq!(container_root(unproven), None, "{unproven}");
    }
}

const TWO_JOBS: &str = "jobs:\n  - command: python cron.py\n    schedule: \"* * * * *\"\n  - command: python /code/cron.py\n    schedule: \"* * * * *\"\n";

// AC-0330: a relative entry is Confirmed only from the final stage's proven
// working directory, never from the copy destination alone. (T-0330)
#[test]
fn relative_entries_need_the_final_working_directory() {
    for dockerfile in [
        "FROM a\nWORKDIR /code\nCOPY . .\nWORKDIR /other\n",
        "FROM a\nWORKDIR /other\nCOPY . /code\n",
    ] {
        let out = extract(&[
            ("Dockerfile", dockerfile),
            ("crontab.yml", TWO_JOBS),
            ("cron.py", ""),
        ]);
        let relative = entry(&out, 0, "crontab.yml");
        assert_eq!(confidence(relative), "InferredStrong", "{dockerfile}");
        assert_eq!(relative.props["entry_path_suffix"], true);
        assert_eq!(confidence(entry(&out, 1, "crontab.yml")), "Confirmed");
    }
    let out = extract(&[
        (
            "Dockerfile",
            "FROM a\nWORKDIR /code\nCOPY . .\nWORKDIR jobs\n",
        ),
        (
            "crontab.yml",
            "jobs:\n  - command: python cleanup.py\n    schedule: \"* * * * *\"\n",
        ),
        ("jobs/cleanup.py", ""),
    ]);
    let nested = entry(&out, 0, "crontab.yml");
    assert_eq!(nested.dst, format!("file:{REPO}@jobs/cleanup.py"));
    assert_eq!(confidence(nested), "Confirmed");
    assert_eq!(
        nested.props["prov"]["evidence"].as_array().unwrap().len(),
        4
    );
}

// AC-0330: a later COPY/ADD (any source, `--from` included) or RUN in the
// final stage that may reach the entry leaves it InferredStrong, marked and
// citing the instruction; writes elsewhere or before the copy keep it
// Confirmed. (T-0330)
#[test]
fn later_writes_may_replace_the_copied_entry() {
    let base = "FROM a\nWORKDIR /code\nCOPY . .\n";
    for (later, kept) in [
        ("COPY --from=builder /replacement /code/cron.py\n", false),
        ("COPY --from=npm /code /code\n", false),
        ("COPY --from=other /code /tmp/../code\n", false),
        ("COPY --from=other /out /code//./\n", false),
        ("COPY --from=other /out static/../cron.py\n", false),
        ("COPY --from=other /out /tmp/../codex\n", true),
        ("ADD build/ ./\n", false),
        ("COPY generated $DEST\n", false),
        ("RUN make\n", false),
        ("RUN echo \\\n  COPY . /elsewhere\n", false),
        ("COPY --from=npm /code/static /code/static/\n", true),
        ("WORKDIR /code\nENV A=b\n", true),
    ] {
        let out = extract(&[
            ("Dockerfile", &format!("{base}{later}")),
            ("crontab.yml", TWO_JOBS),
            ("cron.py", ""),
        ]);
        for index in 0..2 {
            let edge = entry(&out, index, "crontab.yml");
            assert_eq!(edge.dst, format!("file:{REPO}@cron.py"), "{later}");
            assert_eq!(edge.props["container_dir"], "/code", "{later}");
            if kept {
                assert_eq!(confidence(edge), "Confirmed", "{later}");
                assert!(edge.props.get("entry_may_be_replaced_in_image").is_none());
            } else {
                assert_eq!(confidence(edge), "InferredStrong", "{later}");
                assert_eq!(edge.props["entry_may_be_replaced_in_image"], true);
                let cited = edge.props["prov"]["evidence"].as_array().unwrap();
                let last = cited.last().unwrap()["byte_start"].as_u64().unwrap();
                assert_eq!(last, base.len() as u64, "{later}");
            }
        }
    }
    for before in [
        "FROM a\nWORKDIR /code\nRUN x\nCOPY --from=b /y /code/cron.py\nCOPY . .\n",
        "FROM a AS build\nWORKDIR /code\nCOPY . .\nRUN make\nFROM b\nWORKDIR /code\nCOPY . .\n",
    ] {
        let out = extract(&[
            ("Dockerfile", before),
            ("crontab.yml", TWO_JOBS),
            ("cron.py", ""),
        ]);
        assert_eq!(
            confidence(entry(&out, 1, "crontab.yml")),
            "Confirmed",
            "{before}"
        );
    }
}

// AC-0330: each interpreter has a closed argument grammar; inline code and
// options outside it are never read as running a file. (T-0330)
#[test]
fn interpreter_options_outside_the_grammar_are_not_file_execution() {
    let script = |command: &str| {
        let words: Vec<&str> = command.split_whitespace().collect();
        script_of(&words).map(|script| format!("{script:?}"))
    };
    for (command, expected) in [
        ("python -W ignore cron.py", Ok("Path(\"cron.py\")")),
        ("python3.12 -X dev -OO -u cron.py", Ok("Path(\"cron.py\")")),
        ("python -- -odd.py", Ok("Path(\"-odd.py\")")),
        ("python -I -m app.tasks", Ok("Module(\"app.tasks\")")),
        (
            "node --max-old-space-size=512 cron.js",
            Ok("Path(\"cron.js\")"),
        ),
        ("bash -eu run.sh", Ok("Path(\"run.sh\")")),
        ("node -e cron.js", Err(INLINE_CODE)),
        ("node --eval=x cron.js", Err(INLINE_CODE)),
        ("node -pe x cron.js", Err(INLINE_CODE)),
        ("python -c cron.py", Err(INLINE_CODE)),
        ("python3 -uc x cron.py", Err(INLINE_CODE)),
        ("bash -ec run.sh", Err(INLINE_CODE)),
        ("ruby -we x run.rb", Err(INLINE_CODE)),
        ("perl -E x run.pl", Err(INLINE_CODE)),
        ("node -r hook cron.js", Err(UNMODELED_OPTION)),
        ("bash -o pipefail run.sh", Err(UNMODELED_OPTION)),
        ("bash +e run.sh", Err(UNMODELED_OPTION)),
        ("python -Wonce cron.py", Err(UNMODELED_OPTION)),
        ("python --help", Err(UNMODELED_OPTION)),
        ("python -", Err(NO_SCRIPT)),
        ("python -W", Err(NO_SCRIPT)),
        ("gunicorn wsgi:app", Err(NO_SCRIPT)),
    ] {
        assert_eq!(script(command), expected.map(str::to_string), "{command}");
    }
}

// AC-0330 review: a module that names both `a/b.py` and `a/b/__main__.py`
// is ambiguous; a mapping without `schedule` is no job; the content hash
// changes with the job's cron and command. (T-0330)
#[test]
fn ambiguous_modules_unscheduled_mappings_and_edited_jobs() {
    let crontab = "jobs:\n  - name: Module job\n    command: python -m app.tasks\n    schedule: \"*/5 * * * *\"\n  - name: Not a job\n    command: python cron.py\n";
    let out = extract(&[
        ("Dockerfile", DOCKERFILE),
        ("crontab.yml", crontab),
        ("cron.py", ""),
        ("app/tasks.py", ""),
        ("app/tasks/__main__.py", ""),
    ]);
    let schedules = out.nodes.iter().filter(|n| n.label == "Schedule").count();
    assert_eq!(schedules, 1);
    assert_eq!(
        entry(&out, 0, "crontab.yml").props["reason"],
        "schedule module matches several files"
    );
    let single = extract(&[
        ("Dockerfile", DOCKERFILE),
        ("crontab.yml", crontab),
        ("app/tasks.py", ""),
    ]);
    assert_eq!(confidence(entry(&single, 0, "crontab.yml")), "Confirmed");

    let hashes = |crontab: &str| {
        let out = extract(&[
            ("Dockerfile", DOCKERFILE),
            ("crontab.yml", crontab),
            ("cron.py", ""),
            ("jobs/cron.py", ""),
        ]);
        let node = out.nodes.iter().find(|n| n.label == "Schedule").unwrap();
        let edge = entry(&out, 0, "crontab.yml");
        (
            node.props["prov"]["content_hash"].clone(),
            edge.props["prov"]["content_hash"].clone(),
        )
    };
    let job = |command: &str, cron: &str| {
        format!("jobs:\n  - name: J\n    command: {command}\n    schedule: \"{cron}\"\n")
    };
    let base = hashes(&job("python cron.py", "0 0 * * *"));
    assert_eq!(base, hashes(&job("python cron.py", "0 0 * * *")));
    assert_ne!(base.0, hashes(&job("python cron.py", "0 1 * * *")).0);
    let moved = hashes(&job("python jobs/cron.py", "0 0 * * *"));
    assert_ne!(base.0, moved.0);
    assert_ne!(base.1, moved.1);
}

// #779: crontab(5) files are a second reader behind the same node shape.
// Five fields or an `@` macro, then (in `cron.d`) a user, then the command;
// comments, blank and env lines are not jobs; any other line, and a command
// using `%`, is a Gap naming the line. A file with no parsable job, or a
// non-crontab name, declares nothing. Fields are checked against their
// bounds and the schedule and command are kept verbatim. (AC-0332, T-0332)
#[test]
fn crontab_files_become_schedules() {
    let crontab = "# m h dom mon dow command\nSHELL=/bin/sh\n\n*/5 * * * * python /code/cron.py -j stats\n@daily python jobs/cleanup.py\n0 1 * * mon-fri python /code/cron.py -j notify  # weekday\n15 3 * * * date +\\%F > /tmp/x\n30 4 * * * echo hi % there\nthis is not a job line\n";
    let cron_d = "MAILTO=\"\"\n0 2 * * * www-data python /code/cron.py -j delete\n";
    let out = extract(&[
        ("Dockerfile", DOCKERFILE),
        ("deploy/crontab", crontab),
        ("etc/cron.d/app", cron_d),
        ("cron.py", ""),
        ("jobs/cleanup.py", ""),
        ("docs/notes.cron", "just prose\n"),
    ]);
    assert_eq!(out.files, 2);
    let schedules: Vec<&Node> = out.nodes.iter().filter(|n| n.label == "Schedule").collect();
    assert_eq!(schedules.len(), 7);
    assert!(schedules.iter().all(|s| s.props["format"] == "crontab"));
    assert_eq!(schedules[0].props["schedule"], "*/5 * * * *");
    assert_eq!(
        schedules[0].props["command"],
        "python /code/cron.py -j stats"
    );
    assert_eq!(schedules[1].props["schedule"], "@daily");
    let stats = entry(&out, 0, "deploy/crontab");
    assert_eq!(stats.dst, format!("file:{REPO}@cron.py"));
    assert_eq!(confidence(stats), "Confirmed");
    let span = &stats.props["prov"]["evidence"][0];
    let (start, end) = (
        span["byte_start"].as_u64().unwrap() as usize,
        span["byte_end"].as_u64().unwrap() as usize,
    );
    assert_eq!(
        &crontab[start..end],
        "*/5 * * * * python /code/cron.py -j stats"
    );
    assert_eq!(
        entry(&out, 1, "deploy/crontab").dst,
        format!("file:{REPO}@jobs/cleanup.py")
    );
    // The shell drops a trailing comment; the script is still the entry.
    assert_eq!(
        entry(&out, 2, "deploy/crontab").dst,
        format!("file:{REPO}@cron.py")
    );
    for (index, reason) in [
        (3, "shell-composed schedule command"),
        (4, "crontab command uses % (stdin and newlines)"),
        (5, "crontab line is not a schedule entry"),
    ] {
        assert_eq!(
            entry(&out, index, "deploy/crontab").props["reason"],
            reason,
            "{index}"
        );
    }
    let system = entry(&out, 0, "etc/cron.d/app");
    assert_eq!(system.dst, format!("file:{REPO}@cron.py"));
    let node = out
        .nodes
        .iter()
        .find(|n| n.id == format!("sched:{REPO}@etc/cron.d/app#0"))
        .unwrap();
    assert_eq!(node.props["command"], "python /code/cron.py -j delete");

    // The schedule and command are the source text, spacing included; a
    // spaced assignment is an environment line, not a job.
    let spaced = "SHELL = /bin/bash\n0  0 * * *   printf 'a  b' > /dev/null\n";
    let out = extract(&[("crontab", spaced)]);
    let nodes: Vec<&Node> = out.nodes.iter().filter(|n| n.label == "Schedule").collect();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].props["schedule"], "0  0 * * *");
    assert_eq!(nodes[0].props["command"], "printf 'a  b' > /dev/null");

    // Out-of-range values, names outside month/weekday, zero or dangling
    // steps and inverted ranges are not schedule fields.
    for fields in [
        "99 * * * *",
        "* 24 * * *",
        "* * 0 * *",
        "* * * 13 *",
        "* * * * 8",
        "mon * * * *",
        "* * * * jan",
        "*/0 * * * *",
        "*/ * * * *",
        "5/10 * * * *",
        "30-10 * * * *",
        "1,,2 * * * *",
    ] {
        let body = format!("* * * * * python cron.py\n{fields} python cron.py\n");
        let out = extract(&[("crontab", &body), ("cron.py", "")]);
        assert_eq!(
            entry(&out, 1, "crontab").props["reason"],
            "crontab line is not a schedule entry",
            "{fields}"
        );
    }
    for fields in [
        "0-59/15 0-23 1-31 1-12 0-7",
        "* * * JAN-dec Mon-Fri",
        "1,2,30 */2 * feb sun",
        "*/1 * * * 7",
    ] {
        let body = format!("{fields} python cron.py\n");
        let out = extract(&[("crontab", &body), ("cron.py", "")]);
        assert_eq!(
            entry(&out, 0, "crontab").dst,
            format!("file:{REPO}@cron.py"),
            "{fields}"
        );
    }

    for (path, body) in [
        ("crontab", "# only comments\nPATH=/usr/bin\n"),
        ("crontab", "* * * * *\n"),
        ("scripts/crontab.sh", "*/5 * * * * python cron.py\n"),
    ] {
        let out = extract(&[(path, body), ("cron.py", "")]);
        assert_eq!(out.files, 0, "{path}: {body}");
    }
}

// #779: Kubernetes `CronJob` documents are a third reader: one job per
// container, `command` then `args`. Its image is only assumed to be the root
// `Dockerfile` build, so a container-mapped entry is InferredStrong; an
// entrypoint-only container, `workingDir`, a templated manifest or a
// spaced argument is a Gap naming the job. Other kinds declare nothing.
// (AC-0333, T-0333)
#[test]
fn kubernetes_cronjobs_become_schedules() {
    let manifest = r#"apiVersion: v1
kind: ConfigMap
metadata:
  name: settings
---
apiVersion: batch/v1
kind: CronJob
metadata:
  name: stats
spec:
  schedule: "0 * * * *"
  jobTemplate:
    spec:
      template:
        spec:
          containers:
            - name: main
              image: registry.example/app:1
              command: ["python", "/code/cron.py"]
              args:
                - -j
                - stats
            - name: sidecar
              image: registry.example/app:1
              command:
                - python
                - jobs/cleanup.py
            - name: entrypoint
              image: registry.example/app:1
              args: ["--once"]
            - name: shell
              image: registry.example/app:1
              command: ["sh", "-c", "python cron.py && echo done"]
            - name: empty-command
              image: registry.example/app:1
              command: []
              args: ["python", "cron.py"]
            - name: empty-argument
              image: registry.example/app:1
              command: ["python", "", "cron.py"]
            - name: elsewhere
              image: registry.example/app:1
              workingDir: /srv
              command: ["python", "cron.py"]
---
apiVersion: batch/v1
kind: CronJob
metadata:
  name: nightly
spec:
  schedule: "@daily"
  jobTemplate:
    spec:
      template:
        spec:
          containers:
            - name: only
              image: registry.example/app:1
              command: ["python", "-m", "app.tasks"]
"#;
    let out = extract(&[
        ("Dockerfile", DOCKERFILE),
        ("deploy/k8s/cron.yaml", manifest),
        ("cron.py", ""),
        ("jobs/cleanup.py", ""),
        ("app/tasks.py", ""),
        (
            "chart/templates/cron.yaml",
            "apiVersion: batch/v1\nkind: CronJob\nmetadata:\n  name: t\nspec:\n  schedule: \"{{ .Values.cron }}\"\n  jobTemplate:\n    spec:\n      template:\n        spec:\n          containers:\n            - name: t\n              command: [\"python\", \"cron.py\"]\n",
        ),
        (
            "deploy/k8s/job.yaml",
            "apiVersion: batch/v1\nkind: Job\nspec:\n  schedule: \"0 0 * * *\"\n",
        ),
        (
            "deploy/k8s/custom.yaml",
            "apiVersion: example.com/v1\nkind: CronJob\nspec:\n  schedule: \"0 0 * * *\"\n",
        ),
    ]);
    let file = "deploy/k8s/cron.yaml";
    let schedules: Vec<&Node> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Schedule" && n.props["file"] == file)
        .collect();
    assert_eq!(schedules.len(), 8);
    assert!(schedules.iter().all(|s| s.props["format"] == "kubernetes"));
    assert_eq!(schedules[0].props["name"], "stats/main");
    assert_eq!(schedules[0].props["schedule"], "0 * * * *");
    assert_eq!(
        schedules[0].props["command"],
        "python /code/cron.py -j stats"
    );
    assert_eq!(schedules[7].props["name"], "nightly");
    assert_eq!(schedules[7].props["schedule"], "@daily");

    let main = entry(&out, 0, file);
    assert_eq!(main.dst, format!("file:{REPO}@cron.py"));
    assert_eq!(confidence(main), "InferredStrong");
    assert_eq!(main.props["entry_image_assumed_from_dockerfile"], true);
    assert_eq!(main.props["container_dir"], "/code");
    let sidecar = entry(&out, 1, file);
    assert_eq!(sidecar.dst, format!("file:{REPO}@jobs/cleanup.py"));
    assert_eq!(confidence(sidecar), "InferredStrong");
    assert_eq!(
        entry(&out, 7, file).dst,
        format!("file:{REPO}@app/tasks.py")
    );
    for (index, reason) in [
        (2, "CronJob container runs its image entrypoint"),
        (3, "CronJob argument is empty or contains whitespace"),
        (4, "CronJob container runs its image entrypoint"),
        (5, "CronJob argument is empty or contains whitespace"),
        (6, "CronJob container sets workingDir"),
    ] {
        assert_eq!(entry(&out, index, file).props["reason"], reason, "{index}");
    }
    let templated = entry(&out, 0, "chart/templates/cron.yaml");
    assert_eq!(templated.props["reason"], "CronJob manifest is templated");
    assert!(
        !out.nodes
            .iter()
            .any(|n| n.props["file"] == "deploy/k8s/job.yaml"
                || n.props["file"] == "deploy/k8s/custom.yaml")
    );
    assert_eq!(out.files, 2);

    // Without a Dockerfile, a relative path rests on the repository root
    // and an absolute one on a unique suffix, as for the other readers.
    let bare = extract(&[(file, manifest), ("cron.py", ""), ("jobs/cleanup.py", "")]);
    let main = entry(&bare, 0, file);
    assert_eq!(main.props["entry_path_suffix"], true);
    assert!(
        main.props
            .get("entry_image_assumed_from_dockerfile")
            .is_none()
    );
    assert_eq!(
        entry(&bare, 1, file).props["entry_relative_to_repo_root"],
        true
    );
}

// #779: `python cron.py -j stats` runs one branch of the script's
// `__main__` block. When the option, the parsed-args binding and a unique
// literal comparison prove the branch, and its body makes one bare call to
// a module-level function the adapter emitted, ENTRY enters that function
// (InferredStrong, a static reading) and cites the branch; anything less
// keeps the file and names why. (AC-0334, T-0334)
#[test]
fn argument_dispatch_enters_the_selected_function() {
    let cron = r#"import argparse
from app.jobs import imported_job


def stats():
    pass


def notify():
    pass


def twice():
    pass


def twice():
    pass


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("-j", "--job", help="job", type=str)
    parser.add_argument("--mode", dest="chosen")
    args = parser.parse_args()
    with open("x"):
        if args.job == "stats":
            LOG.d("Compute stats")
            stats()
        elif "notify" == args.job:
            notify()
        elif args.job == "both":
            stats()
            notify()
        elif args.job == "imported":
            imported_job()
        elif args.job == "twice":
            twice()
        elif args.job == "dup":
            stats()
        elif args.job == "dup":
            notify()
    if args.chosen == "fast":
        notify()
"#;
    let jobs = [
        "python cron.py -j stats",
        "python cron.py --job=notify",
        "python cron.py --mode fast",
        "python cron.py -j dup",
        "python cron.py -j both",
        "python cron.py -j imported",
        "python cron.py -j twice",
        "python cron.py -j missing",
        "python cron.py -j stats --job notify",
    ];
    let yacron: String = std::iter::once("jobs:\n".to_string())
        .chain(jobs.iter().map(|command| {
            format!("  - name: {command}\n    command: {command}\n    schedule: \"0 0 * * *\"\n")
        }))
        .collect();
    let symbols: BTreeSet<String> = ["stats", "notify", "twice", "imported_job"]
        .iter()
        .map(|name| format!("sym:{REPO}@cron.py#{name}"))
        .collect();
    let out = extract_with(&[("cron.py", cron), ("cron.yaml", &yacron)], &symbols);
    let stats = entry(&out, 0, "cron.yaml");
    assert_eq!(stats.dst, format!("sym:{REPO}@cron.py#stats"));
    assert_eq!(confidence(stats), "InferredStrong");
    assert_eq!(stats.props["entry_argument_dispatch"], true);
    assert_eq!(
        stats.props["dispatch"],
        serde_json::json!({"option": "-j", "value": "stats", "function": "stats", "file": "cron.py"})
    );
    let branch = &stats.props["prov"]["evidence"][1];
    assert_eq!(branch["path"], "cron.py");
    let start = branch["byte_start"].as_u64().unwrap() as usize;
    assert!(cron[start..].starts_with("if args.job == \"stats\":"));
    assert_eq!(
        entry(&out, 1, "cron.yaml").dst,
        format!("sym:{REPO}@cron.py#notify")
    );
    assert_eq!(
        entry(&out, 2, "cron.yaml").dst,
        format!("sym:{REPO}@cron.py#notify")
    );
    // In one `if`/`elif` chain the first matching clause wins.
    assert_eq!(
        entry(&out, 3, "cron.yaml").dst,
        format!("sym:{REPO}@cron.py#stats")
    );
    for (index, job) in jobs.iter().enumerate().skip(4) {
        let edge = entry(&out, index, "cron.yaml");
        assert_eq!(edge.dst, format!("file:{REPO}@cron.py"), "{job}");
        assert!(edge.props.get("dispatch").is_none(), "{job}");
        let reason = if job.contains("--job notify") {
            dispatch::AMBIGUOUS
        } else if job.ends_with("imported") {
            // `app.jobs` is no file in this fixture (AC-0336).
            "imported module is not one repository file"
        } else {
            dispatch::UNPROVEN
        };
        assert_eq!(edge.props["dispatch_unresolved"], reason, "{job}");
    }

    // Value-changing declarations, a chain off the straight-line path or
    // behind an unrelated clause, a write to the parsed arguments, and a
    // rebound or decorated function each prove no dispatch for `-j stats`.
    for (from, to) in [
        ("help=\"job\", type=str)", "type=str.upper)"),
        ("help=\"job\", type=str)", "nargs=1)"),
        ("help=\"job\", type=str)", "action=\"append\")"),
        ("help=\"job\", type=str)", "choices=[\"notify\"])"),
        ("    with open(\"x\"):\n", "    if False:\n"),
        ("    with open(\"x\"):\n", "    for _ in range(1):\n"),
        (
            "        if args.job == \"stats\":",
            "        if args.chosen:\n            pass\n        elif args.job == \"stats\":",
        ),
        (
            "    args = parser.parse_args()\n",
            "    args = parser.parse_args()\n    args.job = \"other\"\n",
        ),
        (
            "    args = parser.parse_args()\n",
            "    args = parser.parse_args()\n    tweak(args)\n",
        ),
        ("\nif __name__", "\nstats = notify\n\nif __name__"),
        ("\ndef stats():", "\n@decorated\ndef stats():"),
        (
            "from app.jobs import imported_job",
            "from app.jobs import imported_job as stats",
        ),
        (
            "    args = parser.parse_args()\n",
            "    args = parser.parse_args()\n    parser.add_argument(\"-j\")\n",
        ),
        // The parser, its constructor and the converter must be the real
        // ones; the selected def must have run before the guard.
        (
            "    args = parser.parse_args()\n",
            "    parser = argparse.ArgumentParser()\n    args = parser.parse_args()\n",
        ),
        (
            "    args = parser.parse_args()\n",
            "    configure(parser)\n    args = parser.parse_args()\n",
        ),
        ("argparse.ArgumentParser()", "make_parser()"),
        ("import argparse\n", "from helpers import argparse\n"),
        ("import argparse\n", "if False:\n    import argparse\n"),
        // An exception alias binds the name and then deletes it; an import
        // binds its top-level package (#796 review).
        (
            "\nif __name__",
            "\ntry:\n    pass\nexcept Exception as stats:\n    pass\n\nif __name__",
        ),
        ("import argparse\n", "import argparse\nimport stats.tools\n"),
        (
            "    parser = argparse.ArgumentParser()\n",
            "    if args_ready:\n        parser = argparse.ArgumentParser()\n",
        ),
        (
            "import argparse\n",
            "import argparse\nfrom helpers import *\n",
        ),
        (
            "\nif __name__",
            "\nstr = lambda value: \"other\"\n\nif __name__",
        ),
        (
            "\ndef stats():\n    pass\n",
            "\nif False:\n    def stats():\n        pass\n",
        ),
    ] {
        assert!(cron.contains(from), "{from}");
        let variant = cron.replacen(from, to, 1);
        let out = extract_with(&[("cron.py", &variant), ("cron.yaml", &yacron)], &symbols);
        let edge = entry(&out, 0, "cron.yaml");
        assert_eq!(edge.dst, format!("file:{REPO}@cron.py"), "{to}");
        assert_eq!(
            edge.props["dispatch_unresolved"],
            dispatch::UNPROVEN,
            "{to}"
        );
    }
    // The declaration must precede the parse that reads it.
    let declaration = "    parser.add_argument(\"-j\", \"--job\", help=\"job\", type=str)\n";
    let late = cron.replacen(declaration, "", 1).replacen(
        "    args = parser.parse_args()\n",
        &format!("    args = parser.parse_args()\n{declaration}"),
        1,
    );
    assert_ne!(late, cron);
    let out = extract_with(&[("cron.py", &late), ("cron.yaml", &yacron)], &symbols);
    assert_eq!(
        entry(&out, 0, "cron.yaml").dst,
        format!("file:{REPO}@cron.py")
    );

    // A def or import after the guard is not bound when the block runs.
    for late in ["\ndef stats():\n    pass\n", "import argparse\n"] {
        let after = format!("{}\n{late}", cron.replacen(late, "", 1));
        assert_ne!(after, format!("{cron}\n{late}"));
        let out = extract_with(&[("cron.py", &after), ("cron.yaml", &yacron)], &symbols);
        assert_eq!(
            entry(&out, 0, "cron.yaml").dst,
            format!("file:{REPO}@cron.py"),
            "{late}"
        );
    }

    // A read of the arguments before they are bound fails at run time.
    let early = cron.replacen(
        "    args = parser.parse_args()\n",
        "    print(args.job)\n    args = parser.parse_args()\n",
        1,
    );
    let out = extract_with(&[("cron.py", &early), ("cron.yaml", &yacron)], &symbols);
    assert_eq!(
        entry(&out, 0, "cron.yaml").dst,
        format!("file:{REPO}@cron.py")
    );

    // Value-preserving keywords and argparse's default dest still prove it,
    // and so does an import that only reads a module named like the function.
    for (from, to) in [
        (
            "import argparse\n",
            "import argparse\nfrom app.stats import helper\nimport stats_lib as other\n",
        ),
        (
            "help=\"job\", type=str)",
            "choices=[\"stats\", \"notify\"], action=\"store\")",
        ),
        (
            "\"-j\", \"--job\", help=\"job\", type=str)",
            "\"--job\", \"-j\", required=True)",
        ),
    ] {
        assert!(cron.contains(from), "{from}");
        let variant = cron.replacen(from, to, 1);
        let out = extract_with(&[("cron.py", &variant), ("cron.yaml", &yacron)], &symbols);
        assert_eq!(
            entry(&out, 0, "cron.yaml").dst,
            format!("sym:{REPO}@cron.py#stats"),
            "{to}"
        );
    }

    // A function the adapter did not emit keeps the file, and so does a
    // script without the `__main__` guard; each names why.
    let out = extract_with(
        &[("cron.py", cron), ("cron.yaml", &yacron)],
        &BTreeSet::new(),
    );
    let unemitted = entry(&out, 0, "cron.yaml");
    assert_eq!(unemitted.dst, format!("file:{REPO}@cron.py"));
    assert_eq!(
        unemitted.props["dispatch_unresolved"],
        "dispatched function not emitted by the Python adapter"
    );
    let unguarded = cron.replace("if __name__ == \"__main__\":", "if True:");
    let out = extract_with(&[("cron.py", &unguarded), ("cron.yaml", &yacron)], &symbols);
    let unguarded = entry(&out, 0, "cron.yaml");
    assert_eq!(unguarded.dst, format!("file:{REPO}@cron.py"));
    assert_eq!(unguarded.props["dispatch_unresolved"], dispatch::UNPROVEN);
    // Behind a Dockerfile the file entry is Confirmed; the dispatch onto a
    // function stays a static reading.
    let docked = "jobs:\n  - name: D\n    command: python /code/cron.py -j stats\n    schedule: \"0 0 * * *\"\n  - name: F\n    command: python /code/cron.py\n    schedule: \"0 0 * * *\"\n";
    let out = extract_with(
        &[
            ("Dockerfile", DOCKERFILE),
            ("cron.py", cron),
            ("cron.yaml", docked),
        ],
        &symbols,
    );
    assert_eq!(confidence(entry(&out, 0, "cron.yaml")), "InferredStrong");
    assert_eq!(confidence(entry(&out, 1, "cron.yaml")), "Confirmed");
    // A command passing no option makes no dispatch claim at all.
    let plain = "jobs:\n  - name: P\n    command: python cron.py\n    schedule: \"0 0 * * *\"\n";
    let out = extract_with(&[("cron.py", cron), ("cron.yaml", plain)], &symbols);
    let plain = entry(&out, 0, "cron.yaml");
    assert_eq!(plain.dst, format!("file:{REPO}@cron.py"));
    assert!(plain.props.get("dispatch_unresolved").is_none());
    assert!(plain.props.get("entry_argument_dispatch").is_none());
}

// AC-0336: a dispatched branch may call a function imported from one
// repository module, or run a local coroutine through `asyncio.run`; the
// `ENTRY` then enters the function where it is defined. (T-0336)
#[test]
fn imported_and_async_dispatch_enters_the_defining_function() {
    let cron = r#"import argparse
import asyncio
from tasks.domains import check_domains, remote_coro
from app.mail import resend
from app.alias import aliased as renamed
from .rel import relative_job
from lib.missing import gone
from lib.twice import twice_defined
from shadow.pkg import shadowed
from lib.cond import conditional


async def check_hibp():
    pass


async def coro():
    pass


def sync_job():
    def inner():
        yield 1
    return inner


def gen_job():
    yield 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("-j", "--job")
    args = parser.parse_args()
    if args.job == "domains":
        check_domains()
    elif args.job == "resend":
        resend()
    elif args.job == "hibp":
        asyncio.run(check_hibp())
    elif args.job == "renamed":
        renamed()
    elif args.job == "relative":
        relative_job()
    elif args.job == "missing":
        gone()
    elif args.job == "twice":
        twice_defined()
    elif args.job == "shadowed":
        shadowed()
    elif args.job == "both":
        asyncio.run(check_hibp())
        check_domains()
    elif args.job == "debug":
        asyncio.run(check_hibp(), debug=True)
    elif args.job == "conditional":
        conditional()
    elif args.job == "bare_coro":
        coro()
    elif args.job == "run_sync":
        asyncio.run(sync_job())
    elif args.job == "generator":
        gen_job()
    elif args.job == "sync":
        sync_job()
    elif args.job == "imported_coro":
        asyncio.run(remote_coro())
    elif args.job == "imported_bare_coro":
        remote_coro()
"#;
    let jobs = [
        "domains",
        "resend",
        "hibp",
        "renamed",
        "relative",
        "missing",
        "twice",
        "shadowed",
        "both",
        "debug",
        "conditional",
        "bare_coro",
        "run_sync",
        "generator",
        "sync",
        "imported_coro",
        "imported_bare_coro",
    ];
    let yacron: String = std::iter::once("jobs:\n".to_string())
        .chain(jobs.iter().map(|job| {
            format!("  - name: {job}\n    command: python cron.py -j {job}\n    schedule: \"0 0 * * *\"\n")
        }))
        .collect();
    let modules = [
        (
            "tasks/domains.py",
            "def check_domains():\n    pass\n\n\nasync def remote_coro():\n    pass\n",
        ),
        ("app/__init__.py", ""),
        ("app/mail.py", "def resend():\n    pass\n"),
        ("app/alias.py", "def aliased():\n    pass\n"),
        (
            "lib/twice.py",
            "def twice_defined():\n    pass\n\n\ndef twice_defined():\n    pass\n",
        ),
        ("shadow.py", ""),
        ("shadow/pkg.py", "def shadowed():\n    pass\n"),
        (
            "lib/cond.py",
            "if FLAG:\n    def conditional():\n        pass\n",
        ),
    ];
    let symbols: BTreeSet<String> = [
        "tasks/domains.py#check_domains",
        "app/mail.py#resend",
        "app/alias.py#aliased",
        "lib/twice.py#twice_defined",
        "shadow/pkg.py#shadowed",
        "lib/cond.py#conditional",
        "cron.py#check_hibp",
        "cron.py#coro",
        "cron.py#sync_job",
        "cron.py#gen_job",
        "tasks/domains.py#remote_coro",
    ]
    .iter()
    .map(|id| format!("sym:{REPO}@{id}"))
    .collect();
    let run = |cron: &str| {
        let mut files = vec![("cron.py", cron), ("cron.yaml", yacron.as_str())];
        files.extend(modules);
        extract_with(&files, &symbols)
    };
    let out = run(cron);

    let domains = entry(&out, 0, "cron.yaml");
    assert_eq!(
        domains.dst,
        format!("sym:{REPO}@tasks/domains.py#check_domains")
    );
    assert_eq!(confidence(domains), "InferredStrong");
    assert_eq!(
        domains.props["dispatch"],
        serde_json::json!({
            "option": "-j", "value": "domains", "function": "check_domains",
            "file": "tasks/domains.py", "module": "tasks.domains",
        })
    );
    let import = &domains.props["prov"]["evidence"][2];
    let start = import["byte_start"].as_u64().unwrap() as usize;
    assert_eq!(import["path"], "cron.py");
    assert!(cron[start..].starts_with("from tasks.domains import check_domains"));
    assert_eq!(
        entry(&out, 1, "cron.yaml").dst,
        format!("sym:{REPO}@app/mail.py#resend")
    );
    let hibp = entry(&out, 2, "cron.yaml");
    assert_eq!(hibp.dst, format!("sym:{REPO}@cron.py#check_hibp"));
    assert!(hibp.props["dispatch"].get("module").is_none());

    // An aliased, relative, unresolvable, shadowed, or doubly or conditionally
    // defined import,
    // or a branch that makes another call or passes `asyncio.run` more,
    // keeps the file and names why.
    for (index, reason) in [
        (3, dispatch::UNPROVEN),
        (4, dispatch::UNPROVEN),
        (5, "imported module is not one repository file"),
        (
            6,
            "imported function is not one top-level def the call runs",
        ),
        (7, "imported module is not one repository file"),
        (8, dispatch::UNPROVEN),
        (9, dispatch::UNPROVEN),
        (
            10,
            "imported function is not one top-level def the call runs",
        ),
        // The call must run the body: `asyncio.run` exactly for an `async
        // def`, and never a generator (#804 review).
        (11, dispatch::UNPROVEN),
        (12, dispatch::UNPROVEN),
        (13, dispatch::UNPROVEN),
        (
            16,
            "imported function is not one top-level def the call runs",
        ),
    ] {
        let edge = entry(&out, index, "cron.yaml");
        assert_eq!(edge.dst, format!("file:{REPO}@cron.py"), "{}", jobs[index]);
        assert_eq!(edge.props["dispatch_unresolved"], reason, "{}", jobs[index]);
    }

    // A `yield` in a nested scope does not make a generator; an imported
    // coroutine runs through `asyncio.run`.
    assert_eq!(
        entry(&out, 14, "cron.yaml").dst,
        format!("sym:{REPO}@cron.py#sync_job")
    );
    assert_eq!(
        entry(&out, 15, "cron.yaml").dst,
        format!("sym:{REPO}@tasks/domains.py#remote_coro")
    );

    // `asyncio` must be the real module, imported before the guard.
    let fake = cron.replacen("import asyncio\n", "from helpers import asyncio\n", 1);
    assert_eq!(
        entry(&run(&fake), 2, "cron.yaml").dst,
        format!("file:{REPO}@cron.py")
    );

    // One statement binding the name twice runs the last item: the alias
    // (#804 review). Same for the module `argparse`.
    let rebound = cron.replacen(
        "from app.mail import resend\n",
        "from app.mail import resend, other as resend\n",
        1,
    );
    assert_eq!(
        entry(&run(&rebound), 1, "cron.yaml").dst,
        format!("file:{REPO}@cron.py")
    );
    let rebound = cron.replacen(
        "import argparse\n",
        "import argparse, json as argparse\n",
        1,
    );
    assert_eq!(
        entry(&run(&rebound), 0, "cron.yaml").dst,
        format!("file:{REPO}@cron.py")
    );

    // A repository module beside the script shadows the standard library:
    // `asyncio` for an awaited call, `argparse` for every one (#804 review).
    let shadowed = |module: &str| {
        let mut files = vec![
            ("cron.py", cron),
            ("cron.yaml", yacron.as_str()),
            (module, ""),
        ];
        files.extend(modules);
        extract_with(&files, &symbols)
    };
    let out = shadowed("asyncio/__init__.py");
    let hibp = entry(&out, 2, "cron.yaml");
    assert_eq!(hibp.dst, format!("file:{REPO}@cron.py"));
    assert_eq!(
        hibp.props["dispatch_unresolved"],
        "standard-library module shadowed by a repository file"
    );
    assert_eq!(
        entry(&out, 0, "cron.yaml").dst,
        format!("sym:{REPO}@tasks/domains.py#check_domains")
    );
    let domains = entry(&shadowed("argparse.py"), 0, "cron.yaml").clone();
    assert_eq!(
        domains.props["dispatch_unresolved"],
        "standard-library module shadowed by a repository file"
    );

    // A module resolves from the script's own directory.
    let files: BTreeSet<String> = ["jobs/run.py", "jobs/helpers/x.py", "helpers/x.py"]
        .iter()
        .map(|path| path.to_string())
        .collect();
    assert_eq!(
        module_file("jobs/run.py", "helpers.x", &files).as_deref(),
        Some("jobs/helpers/x.py")
    );
    assert_eq!(
        module_file("run.py", "helpers.x", &files).as_deref(),
        Some("helpers/x.py")
    );
    let both: BTreeSet<String> = ["m.py", "m/__init__.py"]
        .iter()
        .map(|p| p.to_string())
        .collect();
    assert_eq!(module_file("run.py", "m", &both), None);
}

// #779: each `on.schedule` cron of a GitHub Actions workflow (a YAML file
// directly in `.github/workflows`) is a Schedule whose ENTRY is the
// workflow file itself, Confirmed; its steps are not traced through. A
// cron outside the five-field grammar, a non-literal or aliased one is a
// Gap; workflows without a schedule, and files elsewhere, declare nothing.
// (AC-0335, T-0335)
#[test]
fn actions_schedules_enter_their_workflow() {
    let workflow = r#"name: Nightly sync
on:
  push:
    branches: [main]
  schedule:
    - cron: "15 3 * * 1-5"
    - cron: '0  */6 * * *'
    - cron: "61 * * * *"
    - cron: *nightly
    - when: "0 0 * * *"
    - cron: "20/15 * * * *"
    - !!map
      cron: "0 1 * * *"
  workflow_dispatch:
jobs:
  sync:
    runs-on: ubuntu-latest
    steps:
      - run: python scripts/sync.py
"#;
    let push_only = "name: CI\non: [push]\njobs:\n  test:\n    runs-on: ubuntu-latest\n";
    let out = extract(&[
        (".github/workflows/nightly.yml", workflow),
        (".github/workflows/ci.yaml", push_only),
        (
            ".github/workflows/nested/old.yml",
            "on:\n  schedule:\n    - cron: \"0 0 * * *\"\n",
        ),
        ("scripts/sync.py", ""),
        (
            ".github/workflows/anchored.yml",
            "on:\n  schedule: &nightlies\n    - cron: \"0 0 * * *\"\n",
        ),
        (
            ".github/workflows/tagged.yml",
            "on: !!map\n  schedule:\n    - cron: \"0 0 * * *\"\n",
        ),
    ]);
    let file = ".github/workflows/nightly.yml";
    assert_eq!(out.files, 3);
    let schedules: Vec<&Node> = out.nodes.iter().filter(|n| n.label == "Schedule").collect();
    assert_eq!(schedules.len(), 9);
    assert!(
        schedules
            .iter()
            .all(|s| s.props["format"] == "github-actions")
    );
    let schedules: Vec<&&Node> = schedules
        .iter()
        .filter(|s| s.props["name"] == "Nightly sync")
        .collect();
    assert_eq!(schedules.len(), 7);
    assert_eq!(schedules[0].props["schedule"], "15 3 * * 1-5");
    assert_eq!(schedules[1].props["schedule"], "0  */6 * * *");
    assert_eq!(schedules[5].props["schedule"], "20/15 * * * *");
    // GitHub documents a start-value step (`20/15`); crontab(5) does not.
    assert!(cron_field("20/15", 0, true) && !cron_field("20/15", 0, false));
    assert!(!cron_field("60/15", 0, true) && !cron_field("20/0", 0, true));
    for index in [0, 1, 5] {
        let edge = entry(&out, index, file);
        assert_eq!(edge.dst, format!("file:{REPO}@{file}"));
        assert_eq!(confidence(edge), "Confirmed");
    }
    for (index, reason) in [
        (2, "workflow cron is not five schedule fields"),
        (3, "workflow cron uses a YAML alias, anchor or tag"),
        (4, "workflow schedule entry without a literal cron"),
        // An anchor or tag on the item or a container (#799 review).
        (6, "workflow cron uses a YAML alias, anchor or tag"),
    ] {
        assert_eq!(entry(&out, index, file).props["reason"], reason, "{index}");
    }
    for file in [
        ".github/workflows/anchored.yml",
        ".github/workflows/tagged.yml",
    ] {
        assert_eq!(
            entry(&out, 0, file).props["reason"],
            "workflow cron uses a YAML alias, anchor or tag",
            "{file}"
        );
    }
    // The steps' scripts are not entries.
    assert!(!out.edges.iter().any(|e| e.dst.ends_with("scripts/sync.py")));
}

// AC-1061 (T-1061, #385): over a source capture the schedule reader reads
// the captured bytes of its whole selection and never the checkout: a
// crontab, `CronJob`, script or `Dockerfile` edited, deleted or created
// after capture changes nothing.
#[test]
fn captured_schedule_files_are_read_instead_of_the_checkout() {
    let crontab = "*/5 * * * * python /code/cron.py -j stats\n@daily python jobs/cleanup.py\n0 4 * * * /code/run.sh\n";
    let cronjob = "apiVersion: batch/v1\nkind: CronJob\nmetadata:\n  name: t\nspec:\n  schedule: \"0 * * * *\"\n  jobTemplate:\n    spec:\n      template:\n        spec:\n          containers:\n            - name: t\n              command: [\"python\", \"/code/cron.py\"]\n";
    let dir = tempfile::tempdir().unwrap();
    let write = |files: &[(&str, &str)]| {
        for (path, body) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
    };
    write(&[
        ("Dockerfile", DOCKERFILE),
        ("deploy/crontab", crontab),
        ("k8s/cron.yaml", cronjob),
        ("cron.py", ""),
        ("jobs/cleanup.py", ""),
        ("run.sh", "#!/bin/sh\n"),
        ("README.md", "notes\n"),
    ]);
    let id = SourceId {
        repo: REPO,
        commit: "abc",
    };
    let symbols = BTreeSet::new();
    let facts = |out: &ScheduleExtraction| format!("{:?} {:?} {}", out.nodes, out.edges, out.files);
    let before = facts(&extract_dir(dir.path(), &id, &symbols).unwrap());

    let paths = schedule_paths(dir.path()).unwrap();
    assert_eq!(
        paths,
        [
            "Dockerfile",
            "cron.py",
            "deploy/crontab",
            "jobs/cleanup.py",
            "k8s/cron.yaml",
            "run.sh"
        ]
    );
    let captured: BTreeMap<String, Vec<u8>> = paths
        .iter()
        .map(|rel| (rel.clone(), std::fs::read(dir.path().join(rel)).unwrap()))
        .collect();

    // After capture: edits, a deletion and a new crontab.
    write(&[
        ("Dockerfile", "FROM python:3.12\nWORKDIR /srv\nCOPY . .\n"),
        ("deploy/crontab", "0 0 * * * python other.py\n"),
        ("etc/cron.d/late", "0 2 * * * root python cron.py\n"),
    ]);
    std::fs::remove_file(dir.path().join("jobs/cleanup.py")).unwrap();
    // A target no selector names: its presence is the capture's.
    std::fs::remove_file(dir.path().join("run.sh")).unwrap();
    let read = |rel: &str| {
        captured
            .get(rel)
            .cloned()
            .ok_or_else(|| std::io::Error::other("not captured"))
    };
    let keys: Vec<String> = captured.keys().cloned().collect();
    let after = extract_from_capture(dir.path(), &keys, &read, &id, &symbols).unwrap();
    assert_eq!(facts(&after), before);
    assert_ne!(
        facts(&extract_dir(dir.path(), &id, &symbols).unwrap()),
        before
    );
}
