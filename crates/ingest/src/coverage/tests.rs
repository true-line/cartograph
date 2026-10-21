use super::*;
use crate::preflight::PatternFinding;

fn count(files: u64) -> ProducerCount {
    ProducerCount {
        files,
        nodes: files * 3,
        edges: files * 2,
    }
}

fn observation(
    producer: &'static str,
    input: &'static str,
    declared: &[&str],
    targets: u64,
) -> Observation {
    Observation {
        producer,
        input,
        enriches: "Terraform resource",
        declared: declared.iter().map(|path| path.to_string()).collect(),
        targets,
        selected: true,
    }
}

fn finding(kind: &str, path: &str, line: u64, adapter: Option<&str>) -> PatternFinding {
    PatternFinding {
        kind: kind.into(),
        path: path.into(),
        line,
        message: String::new(),
        detector: crate::preflight::DETECTOR_ID.into(),
        request_adapter: adapter.map(str::to_string),
    }
}

fn state_of<'a>(coverage: &'a RepoCoverage, id: &str) -> (&'a ProducerState, &'a str) {
    let producer = coverage
        .producers
        .iter()
        .find(|producer| producer.id == id)
        .unwrap();
    (&producer.state, producer.stopping_reason.as_str())
}

// AC-0238: every producer reports whether it ran, was excluded, had nothing
// to read or lacked its input, with a reason scoped to the repo, revision
// and layer hints. Unsupported constructs are grouped as tool limitations.
// (T-0238)
#[test]
fn inventory_reports_each_producer_input_and_capability_with_a_scoped_reason() {
    let hints = vec!["infra".to_string()];
    let facts = CoverageFacts {
        repo: "acme/api",
        commit: "0123abc",
        layer_hints: &hints,
        deterministic: vec![
            ("t0.adapter-ts", count(4)),
            ("t0.iac-terraform", count(2)),
            (crate::toolchain::TOOLCHAIN_EXTRACTOR_ID, count(1)),
        ],
        plugins: vec![(
            "plugin.zig@1".into(),
            ProducerCount {
                files: 2,
                nodes: 5,
                edges: 3,
            },
        )],
        observations: vec![
            observation("t1.terraform-state", "state_json", &[], 5),
            observation("t1.pulumi-deployment", "pulumi_json", &[], 0),
            observation("t1.otel-trace", "otel_jsonl", &["traces/a.jsonl"], 3),
        ],
    };
    let report = PreflightReport {
        unsupported: vec![
            finding("uncovered-language", "lib/a.rb", 1, Some("Ruby")),
            finding("uncovered-language", "lib/b.rb", 1, Some("Ruby")),
            finding("inline-eval", "web/x.ts", 9, None),
        ],
        ..PreflightReport::default()
    };
    let coverage = RepoCoverage::from_facts(&facts).with_unsupported(&report);

    assert_eq!(coverage.inventory, INVENTORY_ID);
    assert_eq!(
        state_of(&coverage, "t0.iac-terraform"),
        (
            &ProducerState::Ran,
            "completed over 2 files of acme/api@0123abc for layers infra"
        )
    );
    assert_eq!(
        state_of(&coverage, "t0.adapter-go"),
        (
            &ProducerState::NotSelected,
            "not selected: layer hints [infra] exclude server"
        )
    );
    // Application layers are excluded, so TypeScript ran for Pulumi only
    // and WebExtension manifests were never read.
    let (ts_state, ts_reason) = state_of(&coverage, "t0.adapter-ts");
    assert_eq!(ts_state, &ProducerState::Ran);
    assert!(ts_reason.contains("Pulumi infrastructure bindings only"));
    assert_eq!(
        state_of(&coverage, "t0.webextension"),
        (
            &ProducerState::NotSelected,
            "not selected: layer hints [infra] exclude server and events and client"
        )
    );
    assert_eq!(state_of(&coverage, "plugin.zig@1").0, &ProducerState::Ran);
    let plugin = coverage
        .producers
        .iter()
        .find(|producer| producer.id == "plugin.zig@1")
        .unwrap();
    assert_eq!((plugin.files, plugin.nodes, plugin.edges), (2, 5, 3));

    // Observational producers: missing, not applicable and applied.
    assert_eq!(
        state_of(&coverage, "t1.terraform-state"),
        (
            &ProducerState::MissingInput,
            "no state_json declared for acme/api: 5 Terraform resource facts stay T0 \
             without observed values"
        )
    );
    assert_eq!(
        state_of(&coverage, "t1.pulumi-deployment").0,
        &ProducerState::NotApplicable
    );
    assert_eq!(state_of(&coverage, "t1.otel-trace").0, &ProducerState::Ran);
    assert_eq!(
        coverage.missing_inputs,
        [MissingInput {
            input: "state_json".into(),
            producer: "t1.terraform-state".into(),
            reason: "no state_json declared for acme/api: 5 Terraform resource facts stay T0 \
                     without observed values; declare state_json for this repo in \
                     cartograph.system.toml"
                .into(),
        }]
    );

    assert_eq!(
        coverage
            .unsupported
            .iter()
            .map(|u| (u.capability.as_str(), u.occurrences, u.first_site.as_str()))
            .collect::<Vec<_>>(),
        [("Ruby", 2, "lib/a.rb:1"), ("inline-eval", 1, "web/x.ts:9")]
    );
    assert!(
        coverage.unsupported[0]
            .reason
            .ends_with("a tool limitation, not a system gap")
    );
    assert!(coverage.limited);
}

// AC-0238: an inventory reads as limited whenever anything was excluded,
// missing or unsupported, and only then. (T-0238)
#[test]
fn inventory_is_limited_only_by_recorded_exclusions_missing_inputs_or_unsupported() {
    let facts = CoverageFacts {
        repo: "local/app",
        commit: "workdir",
        layer_hints: &[],
        deterministic: vec![("t0.adapter-ts", count(3))],
        plugins: Vec::new(),
        observations: vec![observation("t1.terraform-state", "state_json", &[], 0)],
    };
    let full = RepoCoverage::from_facts(&facts);
    assert!(!full.limited);
    assert!(
        full.producers
            .iter()
            .all(|producer| producer.state != ProducerState::NotSelected)
    );
    assert_eq!(
        state_of(&full, "t0.adapter-ts").1,
        "completed over 3 files of local/app@workdir"
    );
    assert_eq!(
        state_of(&full, "t0.adapter-python"),
        (
            &ProducerState::NotApplicable,
            "no Python files in local/app@workdir"
        )
    );

    let unsupported = PreflightReport {
        unsupported: vec![finding("uncovered-language", "a.rs", 1, Some("Rust"))],
        ..PreflightReport::default()
    };
    assert!(full.clone().with_unsupported(&unsupported).limited);

    let missing = CoverageFacts {
        observations: vec![observation("t1.terraform-state", "state_json", &[], 1)],
        ..facts.clone()
    };
    assert!(RepoCoverage::from_facts(&missing).limited);

    let hints = vec!["client".to_string()];
    let excluded = CoverageFacts {
        layer_hints: &hints,
        ..facts
    };
    assert!(RepoCoverage::from_facts(&excluded).limited);
}

// AC-0238: an input whose layer the hints exclude is reported as not
// selected, never as applied, even when the manifest declares it. (T-0238)
#[test]
fn excluded_observation_reports_a_declared_input_as_unread() {
    let hints = vec!["server".to_string()];
    let mut state = observation("t1.terraform-state", "state_json", &["s.json"], 4);
    state.selected = false;
    let facts = CoverageFacts {
        repo: "acme/api",
        commit: "0123abc",
        layer_hints: &hints,
        observations: vec![state],
        ..CoverageFacts::default()
    };
    let coverage = RepoCoverage::from_facts(&facts);
    assert_eq!(
        state_of(&coverage, "t1.terraform-state"),
        (
            &ProducerState::NotSelected,
            "not selected: layer hints [server] exclude infra and cloud; declared state_json \
             was not read"
        )
    );
    assert!(coverage.missing_inputs.is_empty());
    assert!(coverage.limited);
}

// AC-0282: declarations the TypeScript adapter left without a primary receipt
// are a named, limiting capability that survives preflight reconciliation;
// none recorded leaves the inventory unchanged.
#[test]
fn receipt_omissions_are_a_named_limitation() {
    let facts = CoverageFacts {
        repo: "local/app",
        commit: "workdir",
        layer_hints: &[],
        deterministic: vec![("t0.adapter-ts", count(3))],
        plugins: Vec::new(),
        observations: Vec::new(),
    };
    let full = RepoCoverage::from_facts(&facts).with_unsupported(&PreflightReport::default());
    assert_eq!(
        full.clone()
            .with_receipt_omissions(&std::collections::BTreeMap::new()),
        full
    );
    let omitted = std::collections::BTreeMap::from([
        ("src/b.ts".to_string(), 2),
        ("src/a.ts".to_string(), 3),
    ]);
    let limited = full.with_receipt_omissions(&omitted);
    assert!(limited.limited);
    let entry = &limited.unsupported[0];
    assert_eq!(
        (
            entry.capability.as_str(),
            entry.occurrences,
            entry.first_site.as_str()
        ),
        (RECEIPT_OMISSION_CAPABILITY, 5, "src/a.ts")
    );
    assert!(
        entry
            .reason
            .starts_with("5 TypeScript declarations in 2 files")
    );
    assert!(
        entry
            .reason
            .ends_with("a tool limitation, not a system gap")
    );
}
