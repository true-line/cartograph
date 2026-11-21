use crate::{Extraction, SourceId, extract_dir};

fn id() -> SourceId<'static> {
    SourceId {
        repo: "qwtm/example",
        commit: "abc123",
    }
}

fn extract(files: &[(&str, &str)]) -> Extraction {
    let dir = tempfile::tempdir().unwrap();
    for (path, text) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    extract_dir(dir.path(), &id()).unwrap()
}

fn calls_from<'a>(out: &'a Extraction, src: &str) -> Vec<&'a core_graph::Edge> {
    out.edges
        .iter()
        .filter(|edge| edge.label == "CALLS" && edge.src == src)
        .collect()
}

fn sym(path_and_name: &str) -> String {
    format!("sym:qwtm/example@{path_and_name}")
}

const EVENT_BUS: &str =
    "export class EventBus {\n    publish(event: string): void { console.log(event); }\n}\n";
const SERVICE: &str = "import { EventBus } from '../event-bus';
export class OrderService {
    constructor(private eventBus: EventBus) {}
    place() { this.eventBus.publish('placed'); }
}
";

// AC-1504: a typed-member call whose type is imported through a directory
// barrel (`export * from`) reaches the declaring file's method, citing the
// barrel's export statement; nested barrels are followed too.
#[test]
fn star_barrel_call_reaches_the_declaring_file() {
    let out = extract(&[
        ("event-bus/event-bus.ts", EVENT_BUS),
        ("event-bus/index.ts", "export * from './bus';\n"),
        ("event-bus/bus.ts", "export * from './event-bus';\n"),
        ("service/order.service.ts", SERVICE),
    ]);
    let calls = calls_from(&out, &sym("service/order.service.ts#OrderService.place"));
    assert_eq!(calls.len(), 1, "{calls:?}");
    let call = calls[0];
    assert_eq!(call.dst, sym("event-bus/event-bus.ts#EventBus.publish"));
    assert_eq!(call.props["resolution"], "re-export");
    assert_eq!(
        call.props["via"],
        serde_json::json!(["event-bus/index.ts", "event-bus/bus.ts"])
    );
    let prov: core_prov::Provenance = serde_json::from_value(call.props["prov"].clone()).unwrap();
    let cited: Vec<_> = prov.evidence.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        cited,
        [
            "service/order.service.ts",
            "event-bus/index.ts",
            "event-bus/bus.ts"
        ]
    );
    assert!(
        !out.nodes
            .iter()
            .any(|node| node.label == "Gap" && node.props["callee"] == "EventBus.publish"),
        "no call Gap remains"
    );
}

// AC-1504: named re-exports, renamed ones included, and `export { a }` of
// an imported binding, resolve plain function calls.
#[test]
fn named_and_local_reexports_resolve_function_calls() {
    let out = extract(&[
        (
            "lib/impl.ts",
            "export function realName() { return 1; }\nexport function other() { return 2; }\n",
        ),
        (
            "lib/index.ts",
            "export { realName as api } from './impl';\nimport { other } from './impl';\nexport { other };\n",
        ),
        (
            "app/main.ts",
            "import { api, other } from '../lib';\nexport function main() { api(); other(); }\n",
        ),
    ]);
    let mut dsts: Vec<_> = calls_from(&out, &sym("app/main.ts#main"))
        .into_iter()
        .map(|edge| {
            (
                edge.dst.clone(),
                edge.props["resolution"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    dsts.sort();
    assert_eq!(
        dsts,
        [
            (sym("lib/impl.ts#other"), "re-export".to_string()),
            (sym("lib/impl.ts#realName"), "re-export".to_string()),
        ]
    );
}

// AC-1504: fail closed. Two `export *` targets declaring the same name, a
// name the barrel never re-exports, and `export * as ns` all keep the
// explicit call Gap.
#[test]
fn ambiguous_or_missing_reexports_keep_the_gap() {
    let out = extract(&[
        ("a/run.ts", "export function run() { return 1; }\n"),
        ("b/run.ts", "export function run() { return 2; }\n"),
        (
            "lib/index.ts",
            "export * from '../a/run';\nexport * from '../b/run';\nexport * as ns from '../a/run';\n",
        ),
        (
            "app/main.ts",
            "import { run, missing, ns } from '../lib';\nexport function main() { run(); missing(); }\n",
        ),
    ]);
    let calls = calls_from(&out, &sym("app/main.ts#main"));
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(
        calls.iter().all(|edge| edge.dst.starts_with("gap:call:")),
        "{calls:?}"
    );
}

// AC-1504: an explicit named export shadows `export *` (ES module
// semantics), so the star target's same-named declaration is not a rival.
#[test]
fn named_reexport_shadows_star_reexport() {
    let out = extract(&[
        ("a/run.ts", "export function run() { return 1; }\n"),
        ("b/run.ts", "export function run() { return 2; }\n"),
        (
            "lib/index.ts",
            "export * from '../a/run';\nexport { run } from '../b/run';\n",
        ),
        (
            "app/main.ts",
            "import { run } from '../lib';\nexport function main() { run(); }\n",
        ),
    ]);
    let calls = calls_from(&out, &sym("app/main.ts#main"));
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].dst, sym("b/run.ts#run"));
}
