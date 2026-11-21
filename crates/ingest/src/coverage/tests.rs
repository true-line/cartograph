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
        commit_note: None,
        layer_hints: &hints,
        deterministic: vec![
            ("t0.adapter-ts", count(4)),
            ("t0.iac-terraform", count(2)),
            (crate::toolchain::TOOLCHAIN_EXTRACTOR_ID, count(1)),
        ],
        plugins: vec![PluginRead {
            id: "plugin.zig@1".into(),
            count: ProducerCount {
                files: 2,
                nodes: 5,
                edges: 3,
            },
            refused: Vec::new(),
        }],
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
            "not selected: layer hints [infra] exclude server and events and client"
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
        commit_note: None,
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

// AC-0458: Python emits client fetch sites and event sites as well as server
// routes, so client- or events-only hints still select its adapter.
#[test]
fn python_adapter_is_selected_by_every_application_layer() {
    for layer in ["server", "events", "client"] {
        let hints = vec![layer.to_string()];
        let facts = CoverageFacts {
            repo: "local/app",
            commit: "workdir",
            commit_note: None,
            layer_hints: &hints,
            deterministic: vec![("t0.adapter-python", count(2))],
            plugins: Vec::new(),
            observations: Vec::new(),
        };
        assert_ne!(
            state_of(&RepoCoverage::from_facts(&facts), "t0.adapter-python").0,
            &ProducerState::NotSelected,
            "{layer}"
        );
    }
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
        commit_note: None,
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
        commit_note: None,
        layer_hints: &[],
        deterministic: vec![("t0.adapter-ts", count(3))],
        plugins: Vec::new(),
        observations: Vec::new(),
    };
    let full = RepoCoverage::from_facts(&facts).with_unsupported(&PreflightReport::default());
    assert_eq!(
        full.clone().with_receipt_omissions(
            ReceiptOmission::Declarations,
            &std::collections::BTreeMap::new()
        ),
        full
    );
    let omitted = std::collections::BTreeMap::from([
        ("src/b.ts".to_string(), 2),
        ("src/a.ts".to_string(), 3),
    ]);
    let limited = full
        .clone()
        .with_receipt_omissions(ReceiptOmission::Declarations, &omitted);
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
    // AC-0292: rule and store-capacity omissions are named separately, each
    // under its own capability, and both limit the inventory (#722).
    let both = full
        .with_receipt_omissions(ReceiptOmission::Rules, &omitted)
        .with_receipt_omissions(ReceiptOmission::StoreCapacity, &omitted);
    assert!(both.limited);
    let named: Vec<_> = both
        .unsupported
        .iter()
        .map(|entry| (entry.capability.as_str(), entry.occurrences))
        .collect();
    assert_eq!(
        named,
        [
            (RULE_RECEIPT_OMISSION_CAPABILITY, 5),
            (STORED_RECEIPT_OMISSION_CAPABILITY, 5)
        ]
    );
    assert!(
        both.unsupported[0]
            .reason
            .starts_with("5 TypeScript rule facts in 2 files")
    );
    // AC-0839: forget and supersession reclaim unpinned receipts (AC-0838),
    // so the reason points at both as the way to free space (#725).
    let store = &both.unsupported[1].reason;
    assert!(store.starts_with("5 receipts in 2 files did not fit the retained receipt store"));
    assert!(store.contains("forgetting a retained source"));
    assert!(store.contains("superseded receipts are reclaimed"));
    assert!(store.contains("receipts that nothing pins"));
    assert!(store.contains("a recovery of an unchanged source frees nothing"));
    assert!(store.contains("share limit"));
    assert!(!store.contains("cannot yet reclaim"));
    assert!(store.ends_with("a tool limitation, not a system gap"));
}

// AC-0677 (T-0677): the documentation reader's counts are recorded with the
// inventory; a skipped or partly read file limits it; a repository with no
// documentation file keeps the inventory it had.
#[test]
fn documentation_counts_are_recorded_and_a_skip_limits_the_inventory() {
    let facts = CoverageFacts {
        repo: "local/app",
        commit: "workdir",
        commit_note: None,
        layer_hints: &[],
        deterministic: vec![("t0.adapter-ts", count(3))],
        plugins: Vec::new(),
        observations: Vec::new(),
    };
    let full = RepoCoverage::from_facts(&facts).with_unsupported(&PreflightReport::default());
    assert!(!full.limited);
    let before = serde_json::to_string(&full).unwrap();
    assert!(!before.contains("documents"));
    assert_eq!(
        full.clone().with_documents(DocumentCoverage::default()),
        full
    );

    let read = DocumentCoverage {
        found: 2,
        read: 2,
        skipped: BTreeMap::new(),
        sections: 5,
        sections_truncated: 0,
    };
    let complete = full.clone().with_documents(read.clone());
    assert!(!complete.limited);
    assert_eq!(complete.documents.as_ref(), Some(&read));
    let stored: RepoCoverage =
        serde_json::from_str(&serde_json::to_string(&complete).unwrap()).unwrap();
    assert_eq!(stored, complete);
    // An inventory recorded before the field existed still reads.
    assert_eq!(serde_json::from_str::<RepoCoverage>(&before).unwrap(), full);

    let skipped = DocumentCoverage {
        found: 3,
        skipped: BTreeMap::from([("too_large".to_string(), 1)]),
        ..read.clone()
    };
    assert!(full.clone().with_documents(skipped).limited);
    let truncated = DocumentCoverage {
        sections_truncated: 1,
        ..read
    };
    assert!(full.with_documents(truncated).limited);
}

// AC-0148 (#1229): symlinks the captured selection skipped are one named,
// bounded limitation (count and first path), never silent coverage.
#[test]
fn unfollowed_links_are_a_named_limitation() {
    let facts = CoverageFacts {
        repo: "local/app",
        commit: "workdir",
        commit_note: None,
        layer_hints: &[],
        deterministic: vec![("t0.adapter-ts", count(3))],
        plugins: Vec::new(),
        observations: Vec::new(),
    };
    let full = RepoCoverage::from_facts(&facts).with_unsupported(&PreflightReport::default());
    assert!(!full.limited);
    assert_eq!(full.clone().with_unfollowed_links(&[]), full);
    let links = ["CLAUDE.md".to_string(), "src/lib".to_string()];
    let limited = full.with_unfollowed_links(&links);
    assert!(limited.limited);
    let entry = &limited.unsupported[0];
    assert_eq!(
        (
            entry.capability.as_str(),
            entry.occurrences,
            entry.first_site.as_str()
        ),
        (UNFOLLOWED_LINK_CAPABILITY, 2, "CLAUDE.md")
    );
    assert!(
        entry
            .reason
            .starts_with("2 symlink(s) in the selected tree were not followed")
    );
}

// AC-0310: old serialized coverage without `commit_note` deserializes with
// `commit_note: None`, new coverage with `commit_note: None` serializes
// byte-identically to the pre-existing shape, and `commit_note` is bounded at
// 256 UTF-8 bytes on a char boundary. (T-0310)
#[test]
fn commit_note_round_trips_and_none_serializes_byte_identically() {
    #[derive(Serialize)]
    struct LegacyRepoCoverage<'a> {
        inventory: &'a str,
        repo: &'a str,
        commit: &'a str,
        layer_hints: &'a [String],
        producers: &'a [ProducerCoverage],
        missing_inputs: &'a [MissingInput],
        unsupported: &'a [UnsupportedCapability],
        limited: bool,
    }

    let facts = CoverageFacts {
        repo: "local/app",
        commit: "0123456789abcdef0123456789abcdef01234567",
        commit_note: None,
        layer_hints: &[],
        deterministic: vec![("t0.adapter-ts", count(1))],
        plugins: Vec::new(),
        observations: Vec::new(),
    };
    let without_note = RepoCoverage::from_facts(&facts);
    assert_eq!(without_note.commit_note, None);

    let serialized = serde_json::to_string(&without_note).unwrap();
    assert!(
        !serialized.contains("commit_note"),
        "`commit_note: None` must omit the field: {serialized}"
    );
    let legacy = LegacyRepoCoverage {
        inventory: &without_note.inventory,
        repo: &without_note.repo,
        commit: &without_note.commit,
        layer_hints: &without_note.layer_hints,
        producers: &without_note.producers,
        missing_inputs: &without_note.missing_inputs,
        unsupported: &without_note.unsupported,
        limited: without_note.limited,
    };
    let legacy_bytes = serde_json::to_vec(&legacy).unwrap();
    assert_eq!(
        serde_json::to_vec(&without_note).unwrap(),
        legacy_bytes,
        "new coverage with `commit_note: None` must serialize byte-identically to legacy JSON"
    );
    let parsed_legacy: RepoCoverage = serde_json::from_slice(&legacy_bytes).unwrap();
    assert_eq!(parsed_legacy, without_note);

    let with_note = RepoCoverage::from_facts(&CoverageFacts {
        commit: "workdir",
        commit_note: Some("3 modified or untracked files"),
        ..facts.clone()
    });
    assert_eq!(
        with_note.commit_note.as_deref(),
        Some("3 modified or untracked files")
    );
    let note_json = serde_json::to_string(&with_note).unwrap();
    let round_trip: RepoCoverage = serde_json::from_str(&note_json).unwrap();
    assert_eq!(round_trip, with_note);

    // 256-byte cap on a UTF-8 char boundary.
    let overlong = format!("{}é", "a".repeat(MAX_COMMIT_NOTE_BYTES - 1));
    assert_eq!(overlong.len(), MAX_COMMIT_NOTE_BYTES + 1);
    let bounded = RepoCoverage::from_facts(&CoverageFacts {
        commit: "workdir",
        commit_note: Some(&overlong),
        ..facts
    });
    let note = bounded.commit_note.unwrap();
    assert!(note.len() <= MAX_COMMIT_NOTE_BYTES);
    assert_eq!(note.len(), MAX_COMMIT_NOTE_BYTES - 1);
}

fn plugin_facts(refused: Vec<(String, RefusalReason)>) -> CoverageFacts<'static> {
    CoverageFacts {
        repo: "acme/api",
        commit: "0123abc",
        deterministic: vec![("t0.adapter-ts", count(1))],
        plugins: vec![PluginRead {
            id: "plugin.zig@0123456789ab".into(),
            count: ProducerCount {
                files: 2,
                nodes: 5,
                edges: 3,
            },
            refused,
        }],
        ..CoverageFacts::default()
    }
}

// AC-0658: the inventory names the plugin, each file it was refused on and
// the reason class, in path order whatever order the refusals arrive in. The
// refused files are not counted as read, and each class is a not-covered
// entry, so the inventory is limited.
#[test]
fn plugin_refusals_are_recorded_by_class_and_limit_the_inventory() {
    let refused = vec![
        ("src/z.zig".to_string(), RefusalReason::BoundExceeded),
        ("src/a.zig".to_string(), RefusalReason::MalformedFact),
        ("lib/m.zig".to_string(), RefusalReason::MalformedFact),
        ("src/g.zig".to_string(), RefusalReason::GuestError),
    ];
    let coverage = RepoCoverage::from_facts(&plugin_facts(refused.clone()));
    let row = coverage
        .producers
        .iter()
        .find(|producer| producer.id == "plugin.zig@0123456789ab")
        .unwrap();
    assert_eq!(row.state, ProducerState::Ran);
    assert_eq!(row.files, 2, "refused files are not counted as read");
    assert_eq!(
        row.stopping_reason,
        "read 2 of the 6 files its extensions claim in acme/api@0123abc; the host refused its \
         extraction of 4 (2 malformed fact, 1 guest error, 1 bound exceeded), and no fact from \
         those files by this plugin joined the graph"
    );
    let refusals = row.refusals.as_ref().expect("refusals recorded");
    assert_eq!(
        (
            refusals.malformed_fact,
            refusals.guest_error,
            refusals.bound_exceeded,
            refusals.omitted
        ),
        (2, 1, 1, 0)
    );
    let listed: Vec<(&str, RefusalReason)> = refusals
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.reason))
        .collect();
    assert_eq!(
        listed,
        [
            ("lib/m.zig", RefusalReason::MalformedFact),
            ("src/a.zig", RefusalReason::MalformedFact),
            ("src/g.zig", RefusalReason::GuestError),
            ("src/z.zig", RefusalReason::BoundExceeded),
        ]
    );

    assert!(coverage.limited);
    let entries: Vec<(&str, u64, &str)> = coverage
        .unsupported
        .iter()
        .map(|entry| {
            (
                entry.capability.as_str(),
                entry.occurrences,
                entry.first_site.as_str(),
            )
        })
        .collect();
    assert_eq!(
        entries,
        [
            ("plugin-malformed-fact", 2, "lib/m.zig"),
            ("plugin-guest-error", 1, "src/g.zig"),
            ("plugin-bound-exceeded", 1, "src/z.zig"),
        ]
    );
    assert_eq!(
        coverage.unsupported[0].reason,
        "2 file(s) were refused for a gated plugin because the plugin emitted a fact the host \
         does not accept; no fact from them by that plugin joined the graph, so that plugin \
         does not cover them: a plugin failure, not a system gap"
    );

    // The record is a function of the refusals alone: another arrival order
    // gives the same bytes, and nothing in it is the host's error text.
    let mut reversed = refused;
    reversed.reverse();
    assert_eq!(
        serde_json::to_string(&coverage).unwrap(),
        serde_json::to_string(&RepoCoverage::from_facts(&plugin_facts(reversed))).unwrap()
    );
    assert_eq!(
        serde_json::to_value(row).unwrap()["refusals"],
        serde_json::json!({
            "malformed_fact": 2,
            "guest_error": 1,
            "bound_exceeded": 1,
            "files": [
                {"path": "lib/m.zig", "reason": "malformed_fact"},
                {"path": "src/a.zig", "reason": "malformed_fact"},
                {"path": "src/g.zig", "reason": "guest_error"},
                {"path": "src/z.zig", "reason": "bound_exceeded"}
            ],
            "omitted": 0
        })
    );

    // The preflight report replaces the not-covered list; the refusals stay.
    let report = PreflightReport {
        unsupported: vec![finding("uncovered-language", "lib/a.rb", 1, Some("Ruby"))],
        ..PreflightReport::default()
    };
    let reconciled = coverage.with_unsupported(&report);
    let capabilities: Vec<&str> = reconciled
        .unsupported
        .iter()
        .map(|entry| entry.capability.as_str())
        .collect();
    assert_eq!(
        capabilities,
        [
            "Ruby",
            "plugin-malformed-fact",
            "plugin-guest-error",
            "plugin-bound-exceeded"
        ]
    );
    assert!(
        reconciled
            .with_unsupported(&PreflightReport::default())
            .limited,
        "a refusal alone limits the inventory"
    );
}

// AC-0658: the listed paths are bounded per plugin by count and by bytes;
// the counts by class stay exact and the rest is counted as omitted.
#[test]
fn plugin_refusal_listing_is_bounded_and_counts_the_rest() {
    let many: Vec<(String, RefusalReason)> = (0..MAX_REFUSED_FILES_LISTED + 44)
        .map(|index| (format!("src/{index:04}.zig"), RefusalReason::GuestError))
        .collect();
    let coverage = RepoCoverage::from_facts(&plugin_facts(many));
    let refusals = coverage.producers.last().unwrap().refusals.clone().unwrap();
    assert_eq!(refusals.guest_error, MAX_REFUSED_FILES_LISTED as u64 + 44);
    assert_eq!(refusals.files.len(), MAX_REFUSED_FILES_LISTED);
    assert_eq!(refusals.omitted, 44);
    assert_eq!(refusals.files[0].path, "src/0000.zig");
    assert_eq!(refusals.files.last().unwrap().path, "src/0255.zig");

    let long: Vec<(String, RefusalReason)> = (0..5)
        .map(|index| {
            (
                format!("{index}/{}", "d".repeat(MAX_REFUSED_PATH_BYTES / 4)),
                RefusalReason::MalformedFact,
            )
        })
        .collect();
    let coverage = RepoCoverage::from_facts(&plugin_facts(long));
    let refusals = coverage.producers.last().unwrap().refusals.clone().unwrap();
    assert_eq!(refusals.malformed_fact, 5);
    assert_eq!(refusals.files.len(), 3);
    assert_eq!(refusals.omitted, 2);
    assert!(
        refusals
            .files
            .iter()
            .map(|file| file.path.len())
            .sum::<usize>()
            <= MAX_REFUSED_PATH_BYTES
    );
    assert_eq!(coverage.unsupported[0].occurrences, 5);
}

// AC-0658: an ingest with no refusal records exactly the inventory it did
// before the refusal record existed, byte for byte, so its inventory hash is
// unchanged; and a stored inventory without the field still reads.
#[test]
fn inventory_without_refusals_keeps_its_bytes_and_old_inventories_still_read() {
    #[derive(Serialize)]
    struct LegacyProducer<'a> {
        id: &'a str,
        kind: ProducerKind,
        covers: &'a str,
        state: ProducerState,
        files: u64,
        nodes: u64,
        edges: u64,
        inputs: &'a [String],
        stopping_reason: &'a str,
    }
    #[derive(Serialize)]
    struct LegacyRepoCoverage<'a> {
        inventory: &'a str,
        repo: &'a str,
        commit: &'a str,
        layer_hints: &'a [String],
        producers: Vec<LegacyProducer<'a>>,
        missing_inputs: &'a [MissingInput],
        unsupported: &'a [UnsupportedCapability],
        limited: bool,
    }

    let coverage = RepoCoverage::from_facts(&plugin_facts(Vec::new()));
    assert!(coverage.producers.iter().all(|p| p.refusals.is_none()));
    let legacy = LegacyRepoCoverage {
        inventory: &coverage.inventory,
        repo: &coverage.repo,
        commit: &coverage.commit,
        layer_hints: &coverage.layer_hints,
        producers: coverage
            .producers
            .iter()
            .map(|producer| LegacyProducer {
                id: &producer.id,
                kind: producer.kind,
                covers: &producer.covers,
                state: producer.state,
                files: producer.files,
                nodes: producer.nodes,
                edges: producer.edges,
                inputs: &producer.inputs,
                stopping_reason: &producer.stopping_reason,
            })
            .collect(),
        missing_inputs: &coverage.missing_inputs,
        unsupported: &coverage.unsupported,
        limited: coverage.limited,
    };
    let legacy_bytes = serde_json::to_vec(&legacy).unwrap();
    let bytes = serde_json::to_vec(&coverage).unwrap();
    assert_eq!(bytes, legacy_bytes);
    assert_eq!(
        core_prov::content_hash(&bytes),
        core_prov::content_hash(&legacy_bytes)
    );
    // The plugin row itself, as it was recorded before this change.
    assert_eq!(
        serde_json::to_string(coverage.producers.last().unwrap()).unwrap(),
        r#"{"id":"plugin.zig@0123456789ab","kind":"deterministic","covers":"gated plugin","state":"ran","files":2,"nodes":5,"edges":3,"inputs":[],"stopping_reason":"completed over the files its extensions claim in acme/api@0123abc"}"#
    );
    assert!(!coverage.limited);
    assert!(coverage.unsupported.is_empty());

    let read: RepoCoverage = serde_json::from_slice(&legacy_bytes).unwrap();
    assert_eq!(read, coverage);
}

// AC-0685 (#1847): the found ADR and API contract readers are catalogued
// rows, and an ADR file too large or not UTF-8 is a named, counted
// limitation on the reader's row and in the not-covered list. (T-0685)
#[test]
fn found_adr_and_contract_readers_are_catalogued_and_unread_adrs_named() {
    let facts = CoverageFacts {
        repo: "acme/api",
        commit: "0123abc",
        commit_note: None,
        layer_hints: &[],
        deterministic: vec![(FOUND_ADR_PRODUCER_ID, count(2))],
        plugins: Vec::new(),
        observations: Vec::new(),
    };
    let full = RepoCoverage::from_facts(&facts).with_unsupported(&PreflightReport::default());
    assert_eq!(
        state_of(&full, FOUND_ADR_PRODUCER_ID),
        (
            &ProducerState::Ran,
            "completed over 2 files of acme/api@0123abc"
        )
    );
    assert_eq!(
        state_of(&full, API_CONTRACT_PRODUCER_ID),
        (
            &ProducerState::NotApplicable,
            "no declared API reference files in acme/api@0123abc"
        )
    );
    assert!(!full.limited);
    assert_eq!(full.clone().with_unread_adrs(&[], &[]), full);

    let limited = full.with_unread_adrs(
        &["docs/adr/huge.md".to_string()],
        &["docs/adr/a.md".to_string(), "docs/adr/b.md".to_string()],
    );
    assert!(limited.limited);
    assert_eq!(
        state_of(&limited, FOUND_ADR_PRODUCER_ID),
        (
            &ProducerState::Ran,
            "read 2 of the 5 ADR and RFC files in acme/api@0123abc; 1 too large and 2 not \
             UTF-8 were not read"
        )
    );
    let entries: Vec<_> = limited
        .unsupported
        .iter()
        .map(|entry| {
            (
                entry.capability.as_str(),
                entry.occurrences,
                entry.first_site.as_str(),
            )
        })
        .collect();
    assert_eq!(
        entries,
        vec![
            (ADR_TOO_LARGE_CAPABILITY, 1, "docs/adr/huge.md"),
            (ADR_NOT_UTF8_CAPABILITY, 2, "docs/adr/a.md"),
        ]
    );

    // Only skipped files: the row still says the reader ran and read none.
    let none = RepoCoverage::from_facts(&CoverageFacts {
        deterministic: Vec::new(),
        ..facts
    })
    .with_unread_adrs(&[], &["docs/adr/a.md".to_string()]);
    assert_eq!(
        state_of(&none, FOUND_ADR_PRODUCER_ID).0,
        &ProducerState::Ran
    );
}

// AC-1306 (#2164): a detected language whose adapter has no producer for an
// extractor family names it as not covered and limits the inventory; a
// Python-only run states nothing new.
#[test]
fn detected_languages_name_the_extractor_families_with_no_producer() {
    let facts = CoverageFacts {
        repo: "local/app",
        commit: "workdir",
        commit_note: None,
        layer_hints: &[],
        deterministic: vec![
            ("t0.adapter-python", count(3)),
            ("t0.adapter-kotlin", count(2)),
        ],
        plugins: Vec::new(),
        observations: Vec::new(),
    };
    let detected =
        |language: &str, files: u64, adapter: &str| crate::preflight::LanguageDetection {
            language: language.into(),
            files,
            adapter: Some(adapter.into()),
            coverage: Vec::new(),
        };
    let python_only = PreflightReport {
        languages: vec![detected("Python", 3, "t0.adapter-python")],
        ..PreflightReport::default()
    };
    let python = RepoCoverage::from_facts(&facts).with_unsupported(&python_only);
    assert!(python.unsupported.is_empty(), "{:?}", python.unsupported);

    let both = PreflightReport {
        languages: vec![
            detected("Kotlin", 2, "t0.adapter-kotlin"),
            detected("Python", 3, "t0.adapter-python"),
        ],
        unsupported: vec![finding("inline-eval", "app.py", 4, None)],
        ..PreflightReport::default()
    };
    let coverage = RepoCoverage::from_facts(&facts).with_unsupported(&both);
    assert_eq!(
        coverage
            .unsupported
            .iter()
            .map(|u| (u.capability.as_str(), u.occurrences, u.first_site.as_str()))
            .collect::<Vec<_>>(),
        [
            ("inline-eval", 1, "app.py:4"),
            (
                "extractor-not-run:endpoint-auth:Kotlin",
                2,
                "t0.adapter-kotlin"
            ),
        ]
    );
    assert!(coverage.limited);
}
