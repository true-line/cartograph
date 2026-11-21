use super::*;

const REPO: &str = "local/app";

fn extract(files: &[(&str, &str)]) -> EntryPointExtraction {
    extract_with(files, &BTreeSet::new(), &BTreeSet::new())
}

fn extract_with(
    files: &[(&str, &str)],
    symbols: &BTreeSet<String>,
    scheduled: &BTreeSet<String>,
) -> EntryPointExtraction {
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
        scheduled,
    )
    .unwrap()
}

fn entry<'a>(out: &'a EntryPointExtraction, file: &str, key: &str) -> &'a Node {
    let id = format!("entry:{REPO}@{file}#{key}");
    out.nodes
        .iter()
        .find(|n| n.id == id)
        .unwrap_or_else(|| panic!("no {id} in {:#?}", out.nodes))
}

fn entry_edge<'a>(out: &'a EntryPointExtraction, file: &str, key: &str) -> &'a Edge {
    let src = format!("entry:{REPO}@{file}#{key}");
    out.edges
        .iter()
        .find(|e| e.src == src && e.label == "ENTRY")
        .unwrap()
}

fn confidence(props: &serde_json::Value) -> &str {
    props["prov"]["confidence_tier"].as_str().unwrap()
}

const MAIL: &str = r#"import argparse
from aiosmtpd.controller import Controller

class MailHandler:
    async def handle_DATA(self, server, session, envelope):
        return forward(envelope)

def forward(envelope):
    pass

def main(port: int):
    controller = Controller(MailHandler(), hostname="0.0.0.0", port=port)
    controller.start()

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("-p", "--port", help="SMTP port", type=int, default=20381)
    args = parser.parse_args()
    main(port=args.port)
"#;

const EVENT_SOURCE: &str = r#"class PostgresEventSource:
    def listen(self, cursor):
        cursor.execute(f"LISTEN {CHANNEL};")
"#;

const LISTENER: &str = r#"from events.event_source import PostgresEventSource

def main():
    PostgresEventSource().listen(None)

if __name__ == "__main__":
    main()
"#;

const RUNNER: &str = r#"import time

def execute():
    time.sleep(10)

if __name__ == "__main__":
    while True:
        execute()
"#;

const SHELL: &str = r#"from IPython import embed

def change_password(user_id, new_password):
    pass

if __name__ == "__main__":
    embed()
"#;

const UNGUARDED_COMMAND: &str = r#"#!/usr/bin/env python3
import argparse

parser = argparse.ArgumentParser(prog="Backfill alias")
args = parser.parse_args()
for batch in range(0, 10):
    print(batch)
"#;

const LIBRARY: &str = r#"import logging

LOG = logging.getLogger(__name__)
logging.basicConfig()

def helper():
    pass
"#;

// AC-0890: every Python module that starts a process is an EntryPoint whose
// kind is read from what the module imports and runs: an SMTP listener with
// its default port, a database LISTEN listener whose LISTEN is in an
// imported repository module, a polling loop, an operator shell, and a
// script under `commands/` without a `__main__` guard. A library module is
// none.
#[test]
fn python_process_starts_are_entry_points_by_kind() {
    let out = extract(&[
        ("email_handler.py", MAIL),
        ("events/__init__.py", ""),
        ("events/event_source.py", EVENT_SOURCE),
        ("event_listener.py", LISTENER),
        ("job_runner.py", RUNNER),
        ("shell.py", SHELL),
        ("commands/backfill.py", UNGUARDED_COMMAND),
        ("commands/replace.py", RUNNER),
        ("app/log.py", LIBRARY),
    ]);
    let mail = entry(&out, "email_handler.py", "main");
    assert_eq!(mail.label, "EntryPoint");
    assert_eq!(mail.props["kind"], "smtp-listener");
    assert_eq!(mail.props["port"], 20381);
    assert_eq!(mail.props["started_by"], "a message delivered over SMTP");
    assert_eq!(
        mail.props["name"],
        "Mail received over SMTP (email_handler.py, port 20381)"
    );
    assert_eq!(confidence(&mail.props), "Confirmed");
    let edge = entry_edge(&out, "email_handler.py", "main");
    assert_eq!(edge.dst, format!("file:{REPO}@email_handler.py"));
    assert_eq!(confidence(&edge.props), "Confirmed");

    let listener = entry(&out, "event_listener.py", "main");
    assert_eq!(listener.props["kind"], "database-listener");
    assert_eq!(listener.props["kind_evidence"], "events/event_source.py");
    let cited: Vec<&str> = listener.props["prov"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(cited, ["event_listener.py", "events/event_source.py"]);

    assert_eq!(
        entry(&out, "job_runner.py", "main").props["kind"],
        "polling-loop"
    );
    assert_eq!(
        entry(&out, "shell.py", "main").props["kind"],
        "operator-shell"
    );
    let command = entry(&out, "commands/backfill.py", "main");
    assert_eq!(command.props["kind"], "command");
    assert_eq!(command.props["how"], "script");
    // An operator script is a command however it loops.
    assert_eq!(
        entry(&out, "commands/replace.py", "main").props["kind"],
        "command"
    );

    assert!(!out.nodes.iter().any(|n| n.id.contains("app/log.py")));
    assert!(
        !out.nodes
            .iter()
            .any(|n| n.label == "EntryPoint" && n.id.contains("events/"))
    );
}

// AC-0890: a file a schedule job already enters is that job's entry, and
// tests and packaging scripts are not entry points of the system.
#[test]
fn scheduled_test_and_packaging_modules_are_not_entry_points() {
    let guarded = "if __name__ == \"__main__\":\n    run()\n";
    let out = extract_with(
        &[
            ("cron.py", guarded),
            ("tests/test_cron.py", guarded),
            ("tests/helpers.py", guarded),
            ("conftest.py", guarded),
            ("setup.py", guarded),
        ],
        &BTreeSet::new(),
        &BTreeSet::from(["cron.py".to_string()]),
    );
    assert!(out.nodes.is_empty(), "{:#?}", out.nodes);
}

// AC-0891: the final Dockerfile stage's command is an entry point. A WSGI
// server names its application module; an interpreter names its script.
#[test]
fn container_command_enters_the_application_module() {
    let out = extract(&[
        (
            "Dockerfile",
            "FROM node AS assets\nCMD [\"node\", \"build.js\"]\nFROM python:3.12\nWORKDIR /code\nCOPY . .\nCMD [\"gunicorn\",\"wsgi:app\",\"-b\",\"0.0.0.0:7777\"]\n",
        ),
        (
            "wsgi.py",
            "from server import create_app\napp = create_app()\n",
        ),
    ]);
    let cmd = entry(&out, "Dockerfile", "cmd");
    assert_eq!(cmd.props["kind"], "http-server");
    assert_eq!(cmd.props["command"], "gunicorn wsgi:app -b 0.0.0.0:7777");
    let edge = entry_edge(&out, "Dockerfile", "cmd");
    assert_eq!(edge.dst, format!("file:{REPO}@wsgi.py"));
    assert_eq!(confidence(&edge.props), "InferredStrong");
    assert_eq!(edge.props["entry_module_relative_to_repo_root"], true);
}

// AC-0892: a start this reader cannot resolve to one repository file is a
// Gap that names the command and why, never dropped.
#[test]
fn unresolved_starts_are_gaps_that_name_them() {
    let out = extract(&[
        (
            "Dockerfile",
            "FROM alpine\nCMD ./run.sh && tail -f /dev/null\n",
        ),
        (
            "Procfile",
            "web: gunicorn app:app\nworker: python worker.py\nrelease: ./migrate\n",
        ),
        ("worker.py", "print('x')\n"),
        (
            "pyproject.toml",
            "[project]\nname = \"x\"\n[project.scripts]\nsync = \"x.sync:main\"\n",
        ),
        (
            "tools/package.json",
            "{\"name\":\"cli\",\"bin\":{\"cli\":\"./bin/cli.js\",\"gone\":\"./missing.js\"}}",
        ),
        ("tools/bin/cli.js", "#!/usr/bin/env node\n"),
    ]);
    let gap = |file: &str, key: &str| {
        let edge = entry_edge(&out, file, key);
        let node = out.nodes.iter().find(|n| n.id == edge.dst).unwrap();
        assert_eq!(node.label, "Gap", "{file}#{key}");
        assert_eq!(confidence(&node.props), "Gap");
        node.props["reason"].as_str().unwrap().to_string()
    };
    assert_eq!(
        gap("Dockerfile", "cmd"),
        "entry point not interpreted: shell-composed command"
    );
    assert!(gap("Procfile", "web").contains("module `app`"));
    assert_eq!(
        entry_edge(&out, "Procfile", "worker").dst,
        format!("file:{REPO}@worker.py")
    );
    assert!(gap("Procfile", "release").contains("`./migrate` is not a repository file"));
    assert!(gap("pyproject.toml", "sync").contains("module `x.sync`"));
    assert_eq!(
        entry_edge(&out, "tools/package.json", "cli").dst,
        format!("file:{REPO}@tools/bin/cli.js")
    );
    assert!(gap("tools/package.json", "gone").contains("missing.js"));
    assert_eq!(
        entry(&out, "pyproject.toml", "sync").props["kind"],
        "command"
    );
}

// AC-0891: a console script enters its function when the Python adapter
// emitted it, and its module file otherwise.
#[test]
fn console_script_enters_its_proven_function() {
    let files = [
        (
            "pyproject.toml",
            "[tool.poetry.scripts]\nsync = \"app.cli:main\"\nother = \"app.cli:absent\"\n",
        ),
        ("app/__init__.py", ""),
        ("app/cli.py", "def main():\n    pass\n"),
    ];
    let symbols = BTreeSet::from([format!("sym:{REPO}@app/cli.py#main")]);
    let out = extract_with(&files, &symbols, &BTreeSet::new());
    assert_eq!(
        entry_edge(&out, "pyproject.toml", "sync").dst,
        format!("sym:{REPO}@app/cli.py#main")
    );
    assert_eq!(
        entry_edge(&out, "pyproject.toml", "other").dst,
        format!("file:{REPO}@app/cli.py")
    );
}

// AC-0891 review (#2011): option values are not the application or the
// script, and a non-ASCII string beside a LISTEN is read safely.
#[test]
fn option_values_are_skipped_and_strings_are_read_by_character() {
    let out = extract(&[
        (
            "Procfile",
            "web: gunicorn --bind 0.0.0.0:8000 -k uvicorn.workers:Worker app:app\nworker: python -W ignore -X dev worker.py\n",
        ),
        ("app.py", "app = None\n"),
        ("worker.py", "print('x')\n"),
        (
            "listener.py",
            "A = \"éééé LISTEN\"\nB = \"LISTEN chan\"\nif __name__ == \"__main__\":\n    run()\n",
        ),
    ]);
    assert_eq!(
        entry_edge(&out, "Procfile", "web").dst,
        format!("file:{REPO}@app.py")
    );
    assert_eq!(
        entry_edge(&out, "Procfile", "worker").dst,
        format!("file:{REPO}@worker.py")
    );
    assert_eq!(
        entry(&out, "listener.py", "main").props["kind"],
        "database-listener"
    );
}

const NODE_CLI: &str = r#"#!/usr/bin/env node
import { homedir } from 'node:os';

function parse(argv) {
  const args = { cmd: argv[0], json: false };
  for (let i = 1; i < argv.length; i++) {
    if (argv[i] === '--json') args.json = true;
  }
  return args;
}

function unrelated(args) {
  if (args.cmd === 'nope') return 1;
}

async function ask(op) {
  return send(op);
}

function line(g) {
  return g.state === 'pending' ? 'wait' : 'done';
}

async function main(argv) {
  if (argv[0] === 'machine') return;
  const args = parse(argv);
  if (args.cmd === 'run') return;
  const res = await request('device.status', { harnesses: args.harnesses });
  if (res.state === 'approved') return;
  const made = rekey(args.home);
  if (made.key_storage === 'hardware') return;
  const answer = await ask(args.cmd === 'status' ? 'status' : 'souls.list');
  console.log(line(answer));
  if (args.sub !== 'rekey') return;
  switch (args.cmd) {
    case 'status':
    case 'souls':
      return;
  }
}

main(process.argv.slice(2));
unrelated({ cmd: 'x' });
"#;

#[test]
fn a_node_scripts_subcommands_are_commands() {
    // AC-0921: a `node` shebang script that dispatches on `process.argv`
    // has one command per subcommand it compares the command line with;
    // flags, `!==` and an unrelated `args` are not commands. A shebang
    // script without dispatch is one process, an operator directory's a
    // command, and a test fixture none.
    let out = extract(&[
        ("hq/main.mjs", NODE_CLI),
        (
            "helpers/signer.mjs",
            "#!/usr/bin/env node\nprocess.stdin.pipe(process.stdout);\n",
        ),
        (
            "scripts/build.mjs",
            "#!/usr/bin/env node\nconsole.log(1);\n",
        ),
        (
            "tests/fixture.mjs",
            "#!/usr/bin/env node\nconsole.log(1);\n",
        ),
        ("lib/plain.mjs", "export const x = 1;\n"),
    ]);
    let commands: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "EntryPoint" && n.props["file"] == "hq/main.mjs")
        .map(|n| n.props["command"].as_str().unwrap())
        .collect();
    assert_eq!(
        commands,
        BTreeSet::from(["machine", "run", "souls", "status"])
    );
    let run = entry(&out, "hq/main.mjs", "run");
    assert_eq!(run.props["kind"], "command");
    assert_eq!(run.props["how"], "subcommand");
    assert_eq!(run.props["name"], "Command (hq/main.mjs: run)");
    assert_eq!(
        entry_edge(&out, "hq/main.mjs", "run").dst,
        format!("file:{REPO}@hq/main.mjs")
    );
    assert_eq!(
        entry(&out, "helpers/signer.mjs", "main").props["kind"],
        "process"
    );
    assert_eq!(
        entry(&out, "scripts/build.mjs", "main").props["kind"],
        "command"
    );
    assert!(!out.nodes.iter().any(|n| n.id.contains("tests/fixture.mjs")));
    // Assignment and destructuring carry the command line; a helper
    // resolves in its own scope.
    let other = extract(&[(
        "bin/tool.mjs",
        r#"#!/usr/bin/env node
let args;
args = process.argv.slice(2);
if (args[0] === 'deploy') {}
const [, , command] = process.argv;
if (command === 'build') {}
function a() {
  function pick(v) { return v; }
  if (pick(process.argv)[2] === 'lint') {}
}
function b() {
  function pick(v) { return 'fixed'; }
  if (pick(process.argv) === 'nope') {}
}
"#,
    )]);
    let commands: BTreeSet<&str> = other
        .nodes
        .iter()
        .filter(|n| n.label == "EntryPoint")
        .map(|n| n.props["command"].as_str().unwrap())
        .collect();
    assert_eq!(commands, BTreeSet::from(["build", "deploy", "lint"]));
    assert!(!out.nodes.iter().any(|n| n.id.contains("lib/plain.mjs")));
}

/// AC-0948: a Cloudflare Worker config enters the module it names, relative
/// to the config, and records the worker name and route.
#[test]
fn a_wrangler_config_enters_its_worker_module() {
    const JSONC: &str = r#"{
  "$schema": "node_modules/wrangler/config-schema.json",
  "name": "events",
  "main": "src/index.ts", // the module Cloudflare runs
  /* routes */
  "routes": [{ "pattern": "events.example.com", "custom_domain": true }],
  "vars": { "URL": "https://example.com/x", "MODE": "strict" },
}"#;
    let out = extract(&[
        ("edge/wrangler.jsonc", JSONC),
        (
            "edge/src/index.ts",
            "const sig = req.headers.get(\"X-Hub-Signature-256\");\n",
        ),
        (
            "other/wrangler.toml",
            "name = \"cron\"\nmain = \"missing.ts\"\n",
        ),
        (
            "deploy/wrangler.toml",
            "name = \"up\"\nmain = \"../edge/src/index.ts\"\n",
        ),
        (
            "escape/wrangler.toml",
            "name = \"out\"\nmain = \"../../x.ts\"\n",
        ),
    ]);
    // A parent-relative module resolves; one leaving the repository is a Gap.
    assert_eq!(
        entry_edge(&out, "deploy/wrangler.toml", "worker").dst,
        format!("file:{REPO}@edge/src/index.ts")
    );
    assert!(
        entry_edge(&out, "escape/wrangler.toml", "worker")
            .dst
            .starts_with("gap:")
    );
    let worker = entry(&out, "edge/wrangler.jsonc", "worker");
    assert_eq!(worker.props["kind"], "edge-worker");
    assert_eq!(worker.props["how"], "Cloudflare Worker");
    assert_eq!(worker.props["worker"], "events");
    assert_eq!(worker.props["route"], "events.example.com");
    assert_eq!(
        worker.props["configured_urls"],
        serde_json::json!({"URL": "https://example.com/x"})
    );
    // The signature header names the webhook sender, cited at its span.
    assert_eq!(worker.props["webhook_from"], "GitHub");
    let spans = worker.props["prov"]["evidence"].as_array().unwrap();
    assert!(
        spans
            .iter()
            .any(|s| s["path"] == "edge/src/index.ts" && s["byte_start"] == 29),
        "{spans:?}"
    );
    let edge = entry_edge(&out, "edge/wrangler.jsonc", "worker");
    assert_eq!(edge.dst, format!("file:{REPO}@edge/src/index.ts"));
    assert_eq!(confidence(&edge.props), "Confirmed");
    let missing = entry_edge(&out, "other/wrangler.toml", "worker");
    assert!(
        missing.dst.starts_with("gap:entry-point:"),
        "{}",
        missing.dst
    );
    assert_eq!(
        entry(&out, "other/wrangler.toml", "worker").props["worker"],
        "cron"
    );
}

/// AC-0961: a Go `func main()` in `package main` is a start by kind,
/// entering `main` when the adapter emitted it; tests, e2e trees, ignored
/// files and other packages start nothing.
#[test]
fn a_go_main_package_is_a_start_by_kind() {
    let symbols = BTreeSet::from([format!("sym:{REPO}@cmd/api/main.go#main")]);
    let out = extract_with(
        &[
            (
                "cmd/api/main.go",
                "package main\n\nimport \"net/http\"\n\nfunc main() {\n\thttp.ListenAndServe(\":8080\", nil)\n}\n",
            ),
            ("cmd/tool/main.go", "package main\n\nfunc main() {}\n"),
            (
                "worker/main.go",
                "// Package main.\npackage main\n\nfunc main() { run() }\n",
            ),
            ("e2e/srv/main.go", "package main\n\nfunc main() {}\n"),
            (
                "gen/main.go",
                "//go:build ignore\n\npackage main\n\nfunc main() {}\n",
            ),
            ("pkg/lib/lib.go", "package lib\n\nfunc main() {}\n"),
            (
                "old/main.go",
                "package main\n\n/*\nfunc main() {}\n*/\nvar doc = `\nfunc main() {\n\thttp.ListenAndServe(\":1\", nil)\n}`\n",
            ),
            ("cmd/api/main_test.go", "package main\n\nfunc main() {}\n"),
        ],
        &symbols,
        &BTreeSet::new(),
    );
    let api = entry(&out, "cmd/api/main.go", "main");
    assert_eq!(api.props["kind"], "http-server");
    assert_eq!(api.props["how"], "func main");
    assert_eq!(
        entry_edge(&out, "cmd/api/main.go", "main").dst,
        format!("sym:{REPO}@cmd/api/main.go#main")
    );
    assert_eq!(
        entry(&out, "cmd/tool/main.go", "main").props["kind"],
        "command"
    );
    assert_eq!(
        entry(&out, "worker/main.go", "main").props["kind"],
        "process"
    );
    // Not emitted by the adapter: the start enters the file.
    assert_eq!(
        entry_edge(&out, "worker/main.go", "main").dst,
        format!("file:{REPO}@worker/main.go")
    );
    let entries: Vec<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "EntryPoint")
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(entries.len(), 3, "{entries:?}");
}
