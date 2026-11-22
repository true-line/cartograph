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

/// AC-0966: Java mains, listeners, `@Scheduled` methods and runners are
/// starts by kind, proven by imports; test sources start nothing.
#[test]
fn java_mains_listeners_schedules_and_runners_are_starts() {
    let app = "src/main/java/com/acme/App.java";
    let symbols = BTreeSet::from([
        format!("sym:{REPO}@{app}#App.main(String[])"),
        format!("sym:{REPO}@src/main/java/com/acme/Orders.java#Orders.onOrder(String)"),
    ]);
    let out = extract_with(
        &[
            (
                app,
                "package com.acme;\nimport org.springframework.boot.SpringApplication;\nimport org.springframework.boot.autoconfigure.SpringBootApplication;\n@SpringBootApplication\npublic class App {\n    public static void main(String[] args) {\n        SpringApplication.run(App.class, args);\n    }\n}\n",
            ),
            (
                "src/main/java/com/acme/web/Hello.java",
                "package com.acme.web;\nimport org.springframework.web.bind.annotation.*;\n@RestController\nclass Hello {}\n",
            ),
            (
                "src/main/java/com/acme/Orders.java",
                "package com.acme;\nimport org.springframework.kafka.annotation.KafkaListener;\nimport org.springframework.amqp.rabbit.annotation.*;\nclass Orders {\n    @KafkaListener(topics = {\"orders\", \"refunds\"}, groupId = \"g\")\n    void onOrder(String m) {}\n    @RabbitListener(queues = \"mail\")\n    void onMail(String m) {}\n    @JmsListener(destination = \"q\")\n    void notProven(String m) {}\n}\n",
            ),
            (
                "src/main/java/com/acme/Jobs.java",
                "package com.acme;\nimport org.springframework.scheduling.annotation.Scheduled;\nimport org.springframework.boot.CommandLineRunner;\nimport org.springframework.boot.ApplicationRunner;\nimport org.springframework.context.annotation.Bean;\nclass Jobs implements CommandLineRunner {\n    @Scheduled(cron = \"0 0 * * * *\")\n    void purge() {}\n    public void run(String... args) {}\n    @Bean\n    ApplicationRunner seed() { return a -> {}; }\n}\n",
            ),
            (
                "tools/Migrate.java",
                "public class Migrate {\n    public static void main(final String... args) {}\n    static void main(int x) {}\n}\n",
            ),
            (
                "src/main/java/com/acme/Old.java",
                "class Old {\n    public static void main(String args[]) {}\n    public void main(String[] args) {}\n}\n",
            ),
            (
                "src/test/java/com/acme/AppTest.java",
                "class AppTest { public static void main(String[] a) {} }\n",
            ),
        ],
        &symbols,
        &BTreeSet::new(),
    );
    let main = entry(&out, app, "App.main");
    assert_eq!(main.props["kind"], "http-server");
    assert_eq!(main.props["how"], "public static void main");
    assert_eq!(
        main.props["kind_evidence"],
        "src/main/java/com/acme/web/Hello.java"
    );
    assert_eq!(
        entry_edge(&out, app, "App.main").dst,
        format!("sym:{REPO}@{app}#App.main(String[])")
    );
    let orders = "src/main/java/com/acme/Orders.java";
    let kafka = entry(&out, orders, "Orders.onOrder");
    assert_eq!(kafka.props["kind"], "queue-consumer");
    assert_eq!(kafka.props["how"], "@KafkaListener");
    assert_eq!(
        kafka.props["sources"],
        serde_json::json!(["orders", "refunds"])
    );
    assert_eq!(
        kafka.props["name"],
        format!("Queue messages ({orders}: Orders.onOrder)")
    );
    assert_eq!(
        entry_edge(&out, orders, "Orders.onOrder").dst,
        format!("sym:{REPO}@{orders}#Orders.onOrder(String)")
    );
    // Not emitted by the adapter: the start enters the file.
    assert_eq!(
        entry_edge(&out, orders, "Orders.onMail").dst,
        format!("file:{REPO}@{orders}")
    );
    let jobs = "src/main/java/com/acme/Jobs.java";
    let purge = entry(&out, jobs, "Jobs.purge");
    assert_eq!(purge.props["kind"], "scheduled-job");
    assert_eq!(purge.props["schedule"], "cron = \"0 0 * * * *\"");
    assert_eq!(
        entry(&out, jobs, "Jobs.run").props["how"],
        "CommandLineRunner"
    );
    assert_eq!(entry(&out, jobs, "Jobs.seed").props["kind"], "startup-task");
    assert_eq!(
        entry(&out, "tools/Migrate.java", "Migrate.main").props["kind"],
        "command"
    );
    assert_eq!(
        entry(&out, "src/main/java/com/acme/Old.java", "Old.main").props["kind"],
        "process"
    );
    let entries: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "EntryPoint")
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(entries.len(), 8, "{entries:?}");
}

/// AC-0966: a Spring Boot main with no Spring web endpoint is a process.
#[test]
fn a_spring_main_without_web_endpoints_is_a_process() {
    let out = extract(&[(
        "src/main/java/Batch.java",
        "import org.springframework.boot.*;\npublic class Batch {\n    public static void main(String[] args) { var app = new SpringApplication(Batch.class); app.run(args); }\n}\n",
    )]);
    let batch = entry(&out, "src/main/java/Batch.java", "Batch.main");
    assert_eq!(batch.props["kind"], "process");
    assert!(batch.props.get("kind_evidence").is_none());
    // `new SpringApplication(…)` runs it as well, so with an endpoint it serves.
    let source = "import org.springframework.boot.*;\npublic class Batch {\n    public static void main(String[] args) { var app = new SpringApplication(Batch.class); app.run(args); }\n}\n";
    let web = "import org.springframework.web.bind.annotation.GetMapping;\nclass Api {}\n";
    let out = extract(&[
        ("src/main/java/Batch.java", source),
        ("src/main/java/Api.java", web),
    ]);
    assert_eq!(
        entry(&out, "src/main/java/Batch.java", "Batch.main").props["kind"],
        "http-server"
    );
}

/// AC-0966: overloaded entry methods are told apart by their signature, a
/// runner's other `run` overloads start nothing, and an `it` package is
/// production source.
#[test]
fn java_overloads_have_their_own_entries_and_it_packages_are_read() {
    let file = "src/main/java/it/acme/Inbox.java";
    let symbols = BTreeSet::from([format!("sym:{REPO}@{file}#Inbox.onMessage(byte[])")]);
    let out = extract_with(
        &[(
            file,
            "package it.acme;\nimport org.springframework.jms.annotation.JmsListener;\nimport org.springframework.boot.CommandLineRunner;\nclass Inbox implements CommandLineRunner {\n    @JmsListener(destination = \"a\")\n    void onMessage(String m) {}\n    @JmsListener(destination = \"b\")\n    void onMessage(byte[] m) {}\n    public void run(String[] args) {}\n    void run(int times) {}\n}\n",
        )],
        &symbols,
        &BTreeSet::new(),
    );
    let text = entry(&out, file, "Inbox.onMessage(String)");
    assert_eq!(text.props["handler"], "Inbox.onMessage(String)");
    assert_eq!(
        entry_edge(&out, file, "Inbox.onMessage(byte[])").dst,
        format!("sym:{REPO}@{file}#Inbox.onMessage(byte[])")
    );
    assert_eq!(entry(&out, file, "Inbox.run").props["kind"], "startup-task");
    let entries = out.nodes.iter().filter(|n| n.label == "EntryPoint").count();
    assert_eq!(entries, 3, "{:#?}", out.nodes);
}

/// AC-1009: Rust binary roots, `#[tauri::command]` functions and tonic
/// service methods are starts by kind; comments, strings, tests, examples
/// and `build.rs` start nothing.
#[test]
fn rust_mains_tauri_commands_and_tonic_methods_are_starts() {
    let symbols = BTreeSet::from([
        format!("sym:{REPO}@src-tauri/src/main.rs#main"),
        format!("sym:{REPO}@src-tauri/src/commands.rs#ipc::open_project"),
        format!("sym:{REPO}@grpc/src/main.rs#MyGreeter.Greeter.say_hello"),
    ]);
    let out = extract_with(
        &[
            (
                "src-tauri/src/main.rs",
                "fn main() {\n    tauri::Builder::default()\n        .invoke_handler(tauri::generate_handler![commands::ipc::open_project])\n        .run(tauri::generate_context!())\n        .unwrap();\n}\n",
            ),
            (
                "src-tauri/src/commands.rs",
                "pub mod ipc {\n    #[tauri::command]\n    pub fn open_project(path: String) -> String { path }\n}\n\n// #[tauri::command] fn commented() {}\npub fn plain() {}\n",
            ),
            (
                "src-tauri/src/more.rs",
                "use tauri::{Manager, command};\n\n/// Lists.\n#[command(rename_all = \"snake_case\")]\nasync fn list_items() -> Vec<String> { vec![] }\n\nmod cli {\n    use clap::command;\n    #[command]\n    fn not_ipc() {}\n}\n",
            ),
            (
                "daemon/src/main.rs",
                "fn main() { run(); }\n\n#[allow(dead_code)]\nfn unused() {\n    tauri::Builder::default();\n    axum::serve(listener, app());\n}\n",
            ),
            (
                "grpc/src/main.rs",
                "use tonic::{transport::Server, Request, Response, Status};\n\npub struct MyGreeter;\n\n#[tonic::async_trait]\nimpl Greeter for MyGreeter {\n    async fn say_hello(&self, request: Request<HelloRequest>) -> Result<Response<HelloReply>, Status> { todo!() }\n    fn helper(&self) -> u8 { 0 }\n}\n\n#[tokio::main]\nasync fn main() {\n    Server::builder().add_service(GreeterServer::new(MyGreeter)).serve(addr).await.unwrap();\n}\n",
            ),
            (
                "web/src/main.rs",
                "#[tokio::main]\nasync fn main() {\n    let listener = tokio::net::TcpListener::bind(\"0.0.0.0:3000\").await.unwrap();\n    axum::serve(listener, app()).await.unwrap();\n}\n",
            ),
            ("cli/src/bin/migrate.rs", "fn main() { run(); }\n"),
            // A binary that only calls its own package's library (#1227).
            (
                "desk/Cargo.toml",
                "[package]\nname = \"desk-shell\"\n\n[lib]\npath = \"src/shell.rs\"\n",
            ),
            (
                "desk/src/shell.rs",
                "// tauri::Builder in a comment states nothing.\npub fn helper() -> Option<tauri::Builder<tauri::Wry>> { None }\n\npub fn run() {\n    tauri::Builder::default().run(tauri::generate_context!()).unwrap();\n}\n",
            ),
            (
                "desk/src/bin/desk.rs",
                "fn main() {\n    // Start it.\n    desk_shell /* the library */ :: run();\n}\n",
            ),
            (
                "desk/src/bin/tool.rs",
                "fn main() { desk_shell::helper(); }\n",
            ),
            (
                "desk/src/bin/other.rs",
                "fn main() { other_shell::run(); }\n",
            ),
            (
                "worker/src/main.rs",
                "// axum::serve(listener, app) is for later.\nfn main() { let s = \"HttpServer::new(\"; work(s); }\n",
            ),
            ("lib/src/lib.rs", "pub fn main() {}\n"),
            ("cli/examples/demo.rs", "fn main() {}\n"),
            ("cli/tests/it/main.rs", "fn main() {}\n"),
            ("cli/build.rs", "fn main() {}\n"),
        ],
        &symbols,
        &BTreeSet::new(),
    );
    let desktop = entry(&out, "src-tauri/src/main.rs", "main");
    assert_eq!(desktop.props["kind"], "desktop-app");
    assert_eq!(desktop.props["how"], "fn main");
    assert_eq!(
        entry_edge(&out, "src-tauri/src/main.rs", "main").dst,
        format!("sym:{REPO}@src-tauri/src/main.rs#main")
    );
    let open = entry(&out, "src-tauri/src/commands.rs", "ipc:ipc::open_project");
    assert_eq!(open.props["kind"], "ipc-command");
    assert_eq!(open.props["command_name"], "open_project");
    assert_eq!(
        entry_edge(&out, "src-tauri/src/commands.rs", "ipc:ipc::open_project").dst,
        format!("sym:{REPO}@src-tauri/src/commands.rs#ipc::open_project")
    );
    assert_eq!(
        entry(&out, "src-tauri/src/more.rs", "ipc:list_items").props["kind"],
        "ipc-command"
    );
    let hello = entry(&out, "grpc/src/main.rs", "grpc:MyGreeter.Greeter.say_hello");
    assert_eq!(hello.props["kind"], "grpc-method");
    assert_eq!(hello.props["service"], "Greeter");
    assert_eq!(
        entry_edge(&out, "grpc/src/main.rs", "grpc:MyGreeter.Greeter.say_hello").dst,
        format!("sym:{REPO}@grpc/src/main.rs#MyGreeter.Greeter.say_hello")
    );
    assert_eq!(
        entry(&out, "grpc/src/main.rs", "main").props["kind"],
        "grpc-server"
    );
    assert_eq!(
        entry(&out, "web/src/main.rs", "main").props["kind"],
        "http-server"
    );
    assert_eq!(
        entry(&out, "cli/src/bin/migrate.rs", "main").props["kind"],
        "command"
    );
    assert_eq!(
        entry(&out, "worker/src/main.rs", "main").props["kind"],
        "process"
    );
    // A binary whose `main` only calls its package library's Tauri
    // function is the desktop app, shown by that function (#1227).
    let delegated = entry(&out, "desk/src/bin/desk.rs", "main");
    assert_eq!(delegated.props["kind"], "desktop-app");
    assert_eq!(delegated.props["kind_evidence"], "desk/src/shell.rs");
    for bin in ["desk/src/bin/tool.rs", "desk/src/bin/other.rs"] {
        assert_eq!(entry(&out, bin, "main").props["kind"], "command", "{bin}");
    }
    // A helper `main` never calls does not decide its kind.
    assert_eq!(
        entry(&out, "daemon/src/main.rs", "main").props["kind"],
        "process"
    );
    let entries: Vec<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "EntryPoint")
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(entries.len(), 12, "{entries:?}");
}

/// AC-1104: Kotlin mains (Spring Boot and Ktor), listeners, `@Scheduled`
/// functions and runners are starts by kind, proven by imports; lookalikes,
/// class members named `main` and test sources start nothing.
#[test]
fn kotlin_mains_listeners_schedules_and_runners_are_starts() {
    let app = "src/main/kotlin/com/acme/App.kt";
    let orders = "src/main/kotlin/com/acme/Orders.kt";
    let symbols = BTreeSet::from([
        format!("sym:{REPO}@{app}#main"),
        format!("sym:{REPO}@{orders}#Orders.onOrder"),
    ]);
    let out = extract_with(
        &[
            (
                app,
                "package com.acme\n\nimport org.springframework.boot.autoconfigure.SpringBootApplication\nimport org.springframework.boot.runApplication\n\n@SpringBootApplication\nclass App\n\nfun main(args: Array<String>) {\n    runApplication<App>(*args)\n}\n",
            ),
            (
                "src/main/kotlin/com/acme/web/Hello.kt",
                "package com.acme.web\n\nimport org.springframework.web.bind.annotation.RestController\n\n@RestController\nclass Hello\n",
            ),
            (
                orders,
                "package com.acme\n\nimport org.springframework.kafka.annotation.KafkaListener\nimport org.springframework.amqp.rabbit.annotation.*\n\nclass Orders {\n    @KafkaListener(topics = [\"orders\", \"refunds\"], groupId = \"g\")\n    fun onOrder(m: String) {}\n\n    @RabbitListener(queues = [\"mail\"])\n    fun onMail(m: String) {}\n\n    @JmsListener(destination = \"q\")\n    fun notProven(m: String) {}\n}\n",
            ),
            (
                "src/main/kotlin/com/acme/Jobs.kt",
                "package com.acme\n\nimport org.springframework.scheduling.annotation.Scheduled\nimport org.springframework.boot.CommandLineRunner\nimport org.springframework.boot.ApplicationRunner\nimport org.springframework.context.annotation.Bean\n\nclass Jobs : CommandLineRunner {\n    @Scheduled(cron = \"0 0 * * * *\")\n    fun purge() {}\n\n    override fun run(vararg args: String?) {}\n\n    fun run(times: Int) {}\n\n    @Bean\n    fun seed(): ApplicationRunner = ApplicationRunner { }\n\n    fun main(args: Array<String>) {}\n\n    companion object {\n        @JvmStatic\n        fun main(args: Array<String>) {}\n    }\n}\n",
            ),
            (
                "src/main/kotlin/com/acme/api/Server.kt",
                "package com.acme.api\n\nimport io.ktor.server.engine.embeddedServer\nimport io.ktor.server.netty.Netty\n\nobject Server {\n    @JvmStatic\n    fun main(vararg args: String) {\n        embeddedServer(Netty, port = 8080) { }.start(wait = true)\n    }\n}\n",
            ),
            (
                "tools/Migrate.kt",
                "fun main() {\n    println(\"migrate\")\n}\n",
            ),
            (
                "src/main/kotlin/com/acme/Fake.kt",
                "package com.acme.fake\n\nfun embeddedServer() {}\n\nfun main(args: Array<Int>) { embeddedServer() }\n",
            ),
            ("src/test/kotlin/com/acme/AppTest.kt", "fun main() {}\n"),
        ],
        &symbols,
        &BTreeSet::new(),
    );
    let main = entry(&out, app, "main");
    assert_eq!(main.props["kind"], "http-server");
    assert_eq!(main.props["how"], "fun main");
    assert_eq!(
        main.props["kind_evidence"],
        "src/main/kotlin/com/acme/web/Hello.kt"
    );
    assert_eq!(
        entry_edge(&out, app, "main").dst,
        format!("sym:{REPO}@{app}#main")
    );
    let server = "src/main/kotlin/com/acme/api/Server.kt";
    let ktor = entry(&out, server, "Server.main");
    assert_eq!(ktor.props["kind"], "http-server");
    assert_eq!(ktor.props["kind_evidence"], server);
    let kafka = entry(&out, orders, "Orders.onOrder");
    assert_eq!(kafka.props["kind"], "queue-consumer");
    assert_eq!(kafka.props["how"], "@KafkaListener");
    assert_eq!(
        kafka.props["sources"],
        serde_json::json!(["orders", "refunds"])
    );
    assert_eq!(
        entry_edge(&out, orders, "Orders.onOrder").dst,
        format!("sym:{REPO}@{orders}#Orders.onOrder")
    );
    assert_eq!(
        entry(&out, orders, "Orders.onMail").props["sources"],
        serde_json::json!(["mail"])
    );
    assert_eq!(
        entry_edge(&out, orders, "Orders.onMail").dst,
        format!("file:{REPO}@{orders}")
    );
    let jobs = "src/main/kotlin/com/acme/Jobs.kt";
    let purge = entry(&out, jobs, "Jobs.purge");
    assert_eq!(purge.props["kind"], "scheduled-job");
    assert_eq!(purge.props["schedule"], "cron = \"0 0 * * * *\"");
    assert_eq!(
        entry(&out, jobs, "Jobs.run").props["how"],
        "CommandLineRunner"
    );
    assert_eq!(entry(&out, jobs, "Jobs.seed").props["kind"], "startup-task");
    assert_eq!(entry(&out, jobs, "Jobs.main").props["kind"], "process");
    assert_eq!(
        entry(&out, "tools/Migrate.kt", "main").props["kind"],
        "command"
    );
    let entries: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "EntryPoint")
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(entries.len(), 9, "{entries:?}");
}

/// AC-1104: a Kotlin main is a server only when it calls the server start;
/// a commented-out or longer-package import is no Spring web evidence; an
/// extension `main` is no JVM main.
#[test]
fn kotlin_mains_need_a_start_call_and_a_parsed_web_import() {
    let app = "src/main/kotlin/com/acme/App.kt";
    let tool = "src/main/kotlin/com/acme/Tool.kt";
    let out = extract_with(
        &[
            (
                app,
                "package com.acme\n\nimport org.springframework.boot.runApplication\n\nclass App\n\nfun main(args: Array<String>) {\n    runApplication<App>(*args)\n}\n",
            ),
            (
                "src/main/kotlin/com/acme/web/Old.kt",
                "package com.acme.web\n\n/*\nimport org.springframework.web.bind.annotation.RestController\n*/\nimport org.springframework.web.bind.annotationx.Thing\n",
            ),
            (
                "src/main/java/com/acme/Legacy.java",
                "package com.acme;\n// import org.springframework.web.bind.annotation.GetMapping;\nclass Legacy {}\n",
            ),
            (
                tool,
                "package com.acme\n\nimport io.ktor.server.engine.embeddedServer\nimport org.springframework.boot.SpringApplication\n\nfun main() {\n    println(SpringApplication::class)\n    val start = ::embeddedServer\n}\n",
            ),
            (
                "src/main/kotlin/com/acme/Ext.kt",
                "package com.acme\n\nclass Service\n\nfun Service.main() {}\n",
            ),
        ],
        &BTreeSet::new(),
        &BTreeSet::new(),
    );
    assert_eq!(entry(&out, app, "main").props["kind"], "process");
    assert_eq!(entry(&out, tool, "main").props["kind"], "process");
    let entries: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "EntryPoint")
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(entries.len(), 2, "{entries:?}");
}

/// AC-1110: Android components a manifest declares enter their class's
/// lifecycle function (else its file), resolved against the manifest's
/// package or the module's Gradle namespace; a library or unresolvable
/// component is a Gap; commented-out, removed and test-manifest components
/// declare nothing; a WorkManager worker's `doWork` is a background job.
#[test]
fn android_components_and_workers_are_starts() {
    let manifest = "app/src/main/AndroidManifest.xml";
    let main = "app/src/main/java/com/acme/app/MainActivity.kt";
    let sync = "app/src/main/java/com/acme/app/sync/SyncService.java";
    let boot = "app/src/main/java/com/acme/app/BootReceiver.kt";
    let worker = "app/src/main/java/com/acme/app/SyncWorker.kt";
    let symbols = BTreeSet::from([
        format!("sym:{REPO}@{main}#MainActivity.onCreate"),
        format!("sym:{REPO}@{sync}#SyncService.onStartCommand(Intent,int,int)"),
        format!("sym:{REPO}@{worker}#SyncWorker.doWork"),
    ]);
    let out = extract_with(
        &[
            (
                manifest,
                r#"<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android"
    xmlns:tools="http://schemas.android.com/tools">
    <!-- <activity android:name=".Old" /> -->
    <application android:name=".App">
        <activity android:name=".MainActivity" android:exported="true">
            <intent-filter>
                <action android:name="android.intent.action.MAIN" />
                <category android:name="android.intent.category.LAUNCHER" />
            </intent-filter>
        </activity>
        <activity android:name=".Split">
            <intent-filter>
                <action android:name="android.intent.action.MAIN" />
            </intent-filter>
            <intent-filter>
                <category android:name="android.intent.category.LAUNCHER" />
            </intent-filter>
        </activity>
        <activity-alias android:name=".Alias" android:targetActivity=".MainActivity" />
        <service android:name=".sync.SyncService" android:exported="false" />
        <receiver android:name="BootReceiver">
            <intent-filter>
                <action android:name="android.intent.action.BOOT_COMPLETED" />
            </intent-filter>
        </receiver>
        <provider android:name="androidx.core.content.FileProvider" android:authorities="x" />
        <service android:name="com.lib.Tracker" tools:node="remove" />
    </application>
</manifest>
"#,
            ),
            (
                "app/build.gradle.kts",
                "android {\n    namespace = \"com.acme.app\"\n}\n",
            ),
            (
                main,
                "package com.acme.app\n\nclass MainActivity : ComponentActivity() {\n    override fun onCreate(state: Bundle?) {}\n}\n",
            ),
            (
                sync,
                "package com.acme.app.sync;\n\npublic class SyncService extends Service {\n    public int onStartCommand(Intent i, int f, int id) { return 0; }\n}\n",
            ),
            (
                boot,
                "package com.acme.app\n\nclass BootReceiver : BroadcastReceiver() {\n    override fun onReceive(c: Context, i: Intent) {}\n}\n",
            ),
            (
                worker,
                "package com.acme.app\n\nimport androidx.work.CoroutineWorker\n\nclass SyncWorker(c: Context, p: WorkerParameters) : CoroutineWorker(c, p) {\n    override suspend fun doWork(): Result = Result.success()\n}\n\nclass Fake : Worker() {\n    fun doWork() {}\n}\n",
            ),
            (
                "app/src/androidTest/AndroidManifest.xml",
                "<manifest package=\"com.acme.test\"><application><activity android:name=\".T\"/></application></manifest>\n",
            ),
            (
                "lib/AndroidManifest.xml",
                "<manifest><application><service android:name=\".Orphan\"/></application></manifest>\n",
            ),
        ],
        &symbols,
        &BTreeSet::new(),
    );
    let activity = entry(&out, manifest, "activity:com.acme.app.MainActivity");
    assert_eq!(activity.props["kind"], "android-activity");
    assert_eq!(activity.props["launcher"], true);
    assert_eq!(activity.props["exported"], true);
    assert_eq!(activity.props["how"], "<activity>");
    // MAIN and LAUNCHER in separate intent filters launch nothing.
    let split = entry(&out, manifest, "activity:com.acme.app.Split");
    assert!(split.props.get("launcher").is_none(), "{:?}", split.props);
    assert_eq!(
        activity.props["name"],
        format!("App launch screen ({manifest}: com.acme.app.MainActivity)")
    );
    assert_eq!(
        entry_edge(&out, manifest, "activity:com.acme.app.MainActivity").dst,
        format!("sym:{REPO}@{main}#MainActivity.onCreate")
    );
    assert_eq!(
        entry_edge(&out, manifest, "service:com.acme.app.sync.SyncService").dst,
        format!("sym:{REPO}@{sync}#SyncService.onStartCommand(Intent,int,int)")
    );
    let receiver = entry(&out, manifest, "receiver:com.acme.app.BootReceiver");
    assert_eq!(receiver.props["kind"], "android-receiver");
    assert_eq!(
        receiver.props["actions"],
        serde_json::json!(["android.intent.action.BOOT_COMPLETED"])
    );
    assert_eq!(
        entry_edge(&out, manifest, "receiver:com.acme.app.BootReceiver").dst,
        format!("file:{REPO}@{boot}")
    );
    let provider = entry_edge(
        &out,
        manifest,
        "provider:androidx.core.content.FileProvider",
    );
    assert!(
        provider.dst.starts_with("gap:entry-point:"),
        "{}",
        provider.dst
    );
    assert!(
        provider.props["reason"]
            .as_str()
            .unwrap()
            .contains("androidx.core.content.FileProvider is not declared")
    );
    let orphan = entry_edge(&out, "lib/AndroidManifest.xml", "service:.Orphan");
    assert!(
        orphan.props["reason"]
            .as_str()
            .unwrap()
            .contains("relative")
    );
    let job = entry(&out, worker, "SyncWorker.doWork");
    assert_eq!(job.props["kind"], "background-job");
    assert_eq!(job.props["how"], "CoroutineWorker");
    assert_eq!(
        entry_edge(&out, worker, "SyncWorker.doWork").dst,
        format!("sym:{REPO}@{worker}#SyncWorker.doWork")
    );
    let entries: BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "EntryPoint")
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(entries.len(), 7, "{entries:#?}");
}

/// AC-1110: a module's namespace is read from its own build file, stated
/// either Gradle way, and build output holds no system manifest.
#[test]
fn manifest_scanner_and_gradle_namespace_read_as_written() {
    assert_eq!(
        android::gradle_namespace("android {\n  namespace 'com.a.b'\n}"),
        Some("com.a.b".to_owned())
    );
    assert_eq!(android::gradle_namespace("namespace = libs.x"), None);
    assert_eq!(
        android::build_files("feature/src/main/AndroidManifest.xml"),
        ["feature/build.gradle.kts", "feature/build.gradle"]
    );
    assert_eq!(
        android::build_files("AndroidManifest.xml"),
        ["build.gradle.kts", "build.gradle"]
    );
    assert!(!android::candidate(
        "app/build/intermediates/AndroidManifest.xml"
    ));
    assert!(android::candidate("app/src/debug/AndroidManifest.xml"));
}
