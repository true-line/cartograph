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
