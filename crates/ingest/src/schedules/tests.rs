use super::*;

const REPO: &str = "local/app";

fn extract(files: &[(&str, &str)]) -> ScheduleExtraction {
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
    assert_eq!(schedules.len(), 6);
    assert!(schedules.iter().all(|s| s.props["format"] == "kubernetes"));
    assert_eq!(schedules[0].props["name"], "stats/main");
    assert_eq!(schedules[0].props["schedule"], "0 * * * *");
    assert_eq!(
        schedules[0].props["command"],
        "python /code/cron.py -j stats"
    );
    assert_eq!(schedules[5].props["name"], "nightly");
    assert_eq!(schedules[5].props["schedule"], "@daily");

    let main = entry(&out, 0, file);
    assert_eq!(main.dst, format!("file:{REPO}@cron.py"));
    assert_eq!(confidence(main), "InferredStrong");
    assert_eq!(main.props["entry_image_assumed_from_dockerfile"], true);
    assert_eq!(main.props["container_dir"], "/code");
    let sidecar = entry(&out, 1, file);
    assert_eq!(sidecar.dst, format!("file:{REPO}@jobs/cleanup.py"));
    assert_eq!(confidence(sidecar), "InferredStrong");
    assert_eq!(
        entry(&out, 5, file).dst,
        format!("file:{REPO}@app/tasks.py")
    );
    for (index, reason) in [
        (2, "CronJob container runs its image entrypoint"),
        (3, "CronJob argument contains whitespace"),
        (4, "CronJob container sets workingDir"),
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
