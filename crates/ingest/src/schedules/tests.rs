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
