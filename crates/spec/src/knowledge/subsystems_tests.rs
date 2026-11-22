// AC-0870: subsystem candidates group files by concept across directories.
use super::*;
use core_graph::StoreRevision;
use core_graph::overlay::OverlayStore;

const REPO: &str = "local/fixture";

fn evidence(path: &str) -> EvidenceRef {
    EvidenceRef {
        repo: REPO.into(),
        path: path.into(),
        byte_start: 0,
        byte_end: 10,
        commit_sha: "abc123".into(),
    }
}

fn base_prov(path: &str) -> serde_json::Value {
    serde_json::json!(
        Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![evidence(path)],
            "t0.fixture",
            path.as_bytes(),
        )
        .unwrap()
    )
}

fn file_id(path: &str) -> String {
    format!("file:{REPO}@{path}")
}

fn symbol_id(path: &str, name: &str) -> String {
    format!("sym:{REPO}@{path}#{name}")
}

#[derive(Default)]
struct Fixture {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Fixture {
    fn edge(&mut self, src: String, dst: String, label: &str) {
        self.edges.push(Edge {
            src,
            dst,
            label: label.into(),
            props: serde_json::json!({}),
        });
    }

    /// A source file and the symbols it defines.
    fn file(&mut self, path: &str, symbols: &[&str]) {
        self.nodes.push(Node {
            id: file_id(path),
            label: "File".into(),
            props: serde_json::json!({ "path": path, "prov": base_prov(path) }),
        });
        for name in symbols {
            self.member(path, name, "Symbol");
        }
    }

    fn member(&mut self, path: &str, name: &str, label: &str) {
        self.nodes.push(Node {
            id: symbol_id(path, name),
            label: label.into(),
            props: serde_json::json!({ "name": name, "prov": base_prov(path) }),
        });
        self.edge(symbol_id(path, name), file_id(path), "DEFINED_IN");
    }

    fn calls(&mut self, from: (&str, &str), to: (&str, &str)) {
        self.edge(symbol_id(from.0, from.1), symbol_id(to.0, to.1), "CALLS");
    }

    fn imports(&mut self, from: &str, to: &str) {
        self.edge(file_id(from), file_id(to), "IMPORTS");
    }

    fn snapshot(&self) -> RevisionedSnapshot {
        RevisionedSnapshot {
            revision: StoreRevision {
                graph_instance: "fixture".into(),
                generation: 1,
            },
            graph: (self.nodes.clone(), self.edges.clone()),
        }
    }
}

const ALIAS: [&str; 4] = [
    "app/api/views/alias.py",
    "app/dashboard/views/alias_transfer.py",
    "app/alias_utils.py",
    "tasks/cleanup_alias.py",
];
const MAILBOX: [&str; 4] = [
    "app/api/views/mailbox.py",
    "app/dashboard/views/mailbox_detail.py",
    "app/mailbox_utils.py",
    "app/admin/mailbox.py",
];
const DOMAIN: [&str; 4] = [
    "app/api/views/custom_domain.py",
    "app/dashboard/views/custom_domain.py",
    "app/custom_domain_utils.py",
    "app/custom_domain_validation.py",
];

/// Three concepts, each filed under an API layer, a dashboard layer and a
/// utility module, as a layered legacy application files them.
fn layered() -> Fixture {
    let mut fixture = Fixture::default();
    for (paths, noun) in [(ALIAS, "alias"), (MAILBOX, "mailbox"), (DOMAIN, "domain")] {
        for path in paths {
            fixture.file(path, &[&format!("get_{noun}"), &format!("delete_{noun}")]);
        }
        // Views call their utility module.
        for path in [paths[0], paths[1], paths[3]] {
            fixture.calls(
                (path, &format!("get_{noun}")),
                (paths[2], &format!("get_{noun}")),
            );
            fixture.imports(path, paths[2]);
        }
    }
    fixture
}

fn groups(report: &SubsystemReport) -> Vec<(String, String, Vec<String>)> {
    report
        .facts
        .iter()
        .filter_map(|fact| match &fact.item {
            OverlayItem::Node(node) => Some((
                node.props["name"].as_str().unwrap().to_owned(),
                node.props["kind"].as_str().unwrap().to_owned(),
                fact.cites
                    .iter()
                    .map(|cite| match cite {
                        BaseRef::Node(id) => id.split_once('@').unwrap().1.to_owned(),
                        BaseRef::Edge(..) => panic!("a group cites its files"),
                    })
                    .collect(),
            )),
            OverlayItem::Edge(_) => None,
        })
        .collect()
}

fn sorted(paths: [&str; 4]) -> Vec<String> {
    let mut paths: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
    paths.sort();
    paths
}

// AC-0870: one group per concept, whatever directory its files are in, named
// by the term its files share.
#[test]
fn files_of_one_concept_are_one_subsystem_across_directories() {
    let base = layered().snapshot();
    let report = derive_subsystems(&base);
    assert_eq!(
        (report.files, report.grouped, report.shared, report.omission),
        (12, 12, 0, None)
    );
    let mut found = groups(&report);
    found.sort();
    assert_eq!(
        found,
        vec![
            ("Alias".to_owned(), "cluster".to_owned(), sorted(ALIAS)),
            (
                "Custom domain".to_owned(),
                "cluster".to_owned(),
                sorted(DOMAIN)
            ),
            ("Mailbox".to_owned(), "cluster".to_owned(), sorted(MAILBOX)),
        ]
    );
}

// AC-0870: every group and membership is a weak semantic candidate naming
// its rule, and each member file has one MEMBER_OF edge citing that file.
#[test]
fn groups_are_weak_candidates_with_one_membership_per_file() {
    let base = layered().snapshot();
    let report = derive_subsystems(&base);
    let mut members = 0;
    for fact in &report.facts {
        let props = match &fact.item {
            OverlayItem::Node(node) => {
                assert_eq!(node.label, "Subsystem");
                assert!(node.id.starts_with("subsystem:cluster:"));
                assert_eq!(node.props["files"], 4);
                &node.props
            }
            OverlayItem::Edge(edge) => {
                members += 1;
                assert_eq!(edge.label, "MEMBER_OF");
                assert!(edge.dst.starts_with("subsystem:cluster:"));
                assert_eq!(
                    fact.cites,
                    BTreeSet::from([BaseRef::Node(edge.src.clone())])
                );
                &edge.props
            }
        };
        let prov: Provenance = serde_json::from_value(props["prov"].clone()).unwrap();
        assert_eq!(prov.tier, Tier::Semantic);
        assert_eq!(prov.confidence_tier, ConfidenceTier::InferredWeak);
        assert_eq!(
            prov.method,
            ProducingMethod::Rule {
                rule: SUBSYSTEM_RULE.into(),
                version: SUBSYSTEM_RULE_VERSION.into(),
            }
        );
        assert!(!prov.evidence.is_empty());
    }
    assert_eq!(members, 12);
}

// AC-0870: the overlay admits the candidates as published (R-INT-1, R-INT-2).
#[test]
fn the_overlay_admits_the_candidates() {
    let base = layered().snapshot();
    let report = derive_subsystems(&base);
    let mut overlay = OverlayStore::open_in_memory().unwrap();
    overlay.bind_instance("fixture").unwrap();
    overlay
        .publish(
            &base,
            core_graph::overlay::OverlayRecipe {
                recipe: SUBSYSTEM_RULE.into(),
                embedding_namespace: None,
                provider_ran: false,
            },
            report.facts,
        )
        .unwrap();
}

// AC-0870: the same base gives the same candidates in any input order, and
// the base is not changed.
#[test]
fn candidates_do_not_depend_on_input_order() {
    let base = layered().snapshot();
    let before = base.clone();
    let report = derive_subsystems(&base);
    assert_eq!(base, before);
    let mut reversed = base.clone();
    reversed.graph.0.reverse();
    reversed.graph.1.reverse();
    assert_eq!(derive_subsystems(&reversed), report);
}

// AC-0870: tests, vendored code and versioned third-party trees are not units.
#[test]
fn test_and_vendored_files_are_not_grouped() {
    let mut fixture = layered();
    fixture.file("tests/test_alias.py", &["test_alias_create"]);
    fixture.file("app/alias_checks.py", &[]);
    fixture.member("app/alias_checks.py", "checks alias", "TestCase");
    fixture.file("static/vendor/alias.js", &["alias"]);
    fixture.file("static/DataTables-1.10.16/alias.js", &["alias"]);
    fixture.file("static/alias.min.js", &["alias"]);
    let report = derive_subsystems(&fixture.snapshot());
    assert_eq!((report.files, report.grouped), (12, 12));
    assert!(excluded("packages/core/e2e/order.e2e-spec.ts"));
    assert!(excluded("src/order.spec.ts") && excluded("types/order.d.ts"));
    assert!(!excluded("app/testing_tools/latest.py") && !excluded("app/contest/a-b.py"));
}

// AC-0870: a file most other files call or import is set aside as shared
// instead of joining, and gluing, the concept groups.
#[test]
fn a_file_used_from_everywhere_is_shared() {
    let mut fixture = layered();
    fixture.file("app/db.py", &["session"]);
    for path in ALIAS.iter().chain(&MAILBOX).chain(&DOMAIN) {
        fixture.imports(path, "app/db.py");
    }
    let report = derive_subsystems(&fixture.snapshot());
    assert_eq!((report.files, report.grouped, report.shared), (13, 12, 1));
    let shared: Vec<_> = groups(&report)
        .into_iter()
        .filter(|(_, kind, _)| kind == "shared")
        .collect();
    assert_eq!(
        shared,
        vec![(
            "Shared code".to_owned(),
            "shared".to_owned(),
            vec!["app/db.py".to_owned()]
        )]
    );
}

// AC-1005 (#2083): an errors or exceptions module used from everywhere names
// a mechanism, not a subject, so it is shared code, not one concept's core.
#[test]
fn a_widely_used_errors_module_is_shared() {
    for module in ["app/errors.py", "app/exceptions.py"] {
        let mut fixture = layered();
        fixture.file(module, &["AliasInTrashError"]);
        for path in ALIAS.iter().chain(&MAILBOX).chain(&DOMAIN) {
            fixture.imports(path, module);
        }
        let report = derive_subsystems(&fixture.snapshot());
        assert_eq!(
            (report.files, report.grouped, report.shared),
            (13, 12, 1),
            "{module}"
        );
        let shared = groups(&report)
            .into_iter()
            .find(|(_, kind, _)| kind == "shared")
            .unwrap();
        assert_eq!(shared.2, vec![module.to_owned()]);
    }
}

// AC-0914: a file used from everywhere that is named for a subject is that
// subject's core, not shared code: it stays in the group it names.
#[test]
fn a_widely_used_file_named_for_a_subject_stays_with_it() {
    let mut fixture = layered();
    fixture.file("app/alias_suffix.py", &["get_alias"]);
    for path in ALIAS.iter().chain(&MAILBOX).chain(&DOMAIN) {
        fixture.imports(path, "app/alias_suffix.py");
    }
    let report = derive_subsystems(&fixture.snapshot());
    assert_eq!((report.files, report.grouped, report.shared), (13, 13, 0));
    let alias = groups(&report)
        .into_iter()
        .find(|(name, _, _)| name == "Alias")
        .unwrap();
    assert!(alias.2.contains(&"app/alias_suffix.py".to_owned()));
    assert_eq!(alias.2.len(), 5);
}

// AC-1042: a repository past the old bound of 8,192 unit files is grouped,
// and a signal carried by more than 4,096 files spreads no similarity, so a
// word most of two concepts' files carry does not glue them into one.
#[test]
fn a_repository_past_eight_thousand_files_is_grouped() {
    let mut fixture = Fixture::default();
    for (subject, symbol) in [
        ("alias", "forward"),
        ("mailbox", "verify"),
        ("invoice", "charge"),
    ] {
        for i in 0..3000 {
            let path = format!("app/{subject}/{subject}_{i}.py");
            // 4,200 of the 9,000 files also define a `ledger` symbol.
            let ledger = subject != "invoice" && i < 2100;
            let symbols: &[&str] = if ledger {
                &[symbol, "ledger"]
            } else {
                &[symbol]
            };
            fixture.file(&path, symbols);
        }
    }
    let report = derive_subsystems(&fixture.snapshot());
    assert_eq!(report.omission, None);
    assert_eq!(report.files, 9000);
    let groups = groups(&report);
    for subject in ["Alias", "Mailbox", "Invoice"] {
        let (_, _, files) = groups
            .iter()
            .find(|(name, _, _)| name == subject)
            .unwrap_or_else(|| {
                panic!(
                    "{subject}: {:?}",
                    groups.iter().map(|g| (&g.0, g.2.len())).collect::<Vec<_>>()
                )
            });
        assert_eq!(files.len(), 3000, "{subject}");
    }
}

// AC-0870: a base past the file bound derives nothing and says why; a base
// with nothing to separate derives nothing.
#[test]
fn a_base_past_the_file_bound_is_a_named_omission() {
    let mut fixture = Fixture::default();
    for i in 0..=MAX_SUBSYSTEM_FILES {
        fixture.file(&format!("app/f{i}.py"), &["run"]);
    }
    let report = derive_subsystems(&fixture.snapshot());
    assert_eq!(report.omission, Some(SUBSYSTEM_FILE_CAP));
    assert_eq!(report.files, MAX_SUBSYSTEM_FILES + 1);
    assert!(report.facts.is_empty());

    let mut lone = Fixture::default();
    lone.file("app/main.py", &["main"]);
    assert_eq!(
        derive_subsystems(&lone.snapshot()),
        SubsystemReport {
            files: 1,
            ..SubsystemReport::default()
        }
    );
    assert_eq!(
        derive_subsystems(&Fixture::default().snapshot()),
        SubsystemReport::default()
    );
}

// AC-0870: a plural folds onto its singular only when the corpus has both.
#[test]
fn plurals_fold_onto_a_singular_the_corpus_uses() {
    let vocabulary: BTreeSet<String> = ["domain", "alias", "policy", "box"]
        .map(str::to_owned)
        .into();
    assert_eq!(fold("domains", &vocabulary).as_deref(), Some("domain"));
    assert_eq!(fold("policies", &vocabulary).as_deref(), Some("policy"));
    assert_eq!(fold("boxes", &vocabulary).as_deref(), Some("box"));
    assert_eq!(fold("alias", &vocabulary), None);
    assert_eq!(fold("status", &vocabulary), None);
    assert_eq!(
        subject_words("getCustomDomainById"),
        vec!["custom".to_owned(), "domain".to_owned()]
    );
}

// AC-0870: a local source's registry key is random per registration; the
// groups follow file paths, not that key.
#[test]
fn groups_do_not_depend_on_the_registry_key() {
    let base = layered().snapshot();
    let renamed: (Vec<Node>, Vec<Edge>) = serde_json::from_str(
        &serde_json::to_string(&base.graph)
            .unwrap()
            .replace(REPO, "local/zz-another-registration"),
    )
    .unwrap();
    let other = RevisionedSnapshot {
        revision: base.revision.clone(),
        graph: renamed,
    };
    assert_eq!(
        groups(&derive_subsystems(&other)),
        groups(&derive_subsystems(&base))
    );
}

const CLIENT: &str = "local/client";
const CLIENT_ROOT: &str = "app/src/main/java/io/simplelogin/android";

/// The layered server beside a client repository that names the same three
/// concepts under its own package root.
fn server_and_client() -> RevisionedSnapshot {
    let mut client = Fixture::default();
    for noun in ["alias", "mailbox", "domain"] {
        let paths = [
            format!("{CLIENT_ROOT}/module/{noun}/{noun}_list_fragment.kt"),
            format!("{CLIENT_ROOT}/module/{noun}/{noun}_view_model.kt"),
            format!("{CLIENT_ROOT}/utils/model/{noun}.kt"),
            format!("{CLIENT_ROOT}/utils/network/{noun}_api.kt"),
        ];
        for path in &paths {
            client.file(path, &[&format!("get_{noun}"), &format!("delete_{noun}")]);
        }
        for path in [&paths[0], &paths[1], &paths[3]] {
            client.imports(path, &paths[2]);
        }
    }
    let client: (Vec<Node>, Vec<Edge>) = serde_json::from_str(
        &serde_json::to_string(&client.snapshot().graph)
            .unwrap()
            .replace(REPO, CLIENT),
    )
    .unwrap();
    let mut base = layered().snapshot();
    base.graph.0.extend(client.0);
    base.graph.1.extend(client.1);
    base
}

// AC-0919: files of two repositories are never one group, and a word every
// group of a repository carries (its package root) names none of them.
#[test]
fn a_group_holds_files_of_one_repository() {
    let report = derive_subsystems(&server_and_client());
    let groups = groups(&report);
    assert_eq!((report.files, report.grouped), (24, 24), "{groups:?}");
    assert_eq!(groups.len(), 6, "{groups:?}");
    for (name, _, members) in &groups {
        let client = members
            .iter()
            .filter(|p| p.starts_with(CLIENT_ROOT))
            .count();
        assert!(
            client == 0 || client == members.len(),
            "{name}: {members:?}"
        );
        let lower = name.to_lowercase();
        let title = lower.split(" (").next().unwrap();
        assert!(
            !title.contains("simplelogin") && !title.contains("android"),
            "{name}"
        );
    }
    // The server's groups are what they are without the client; a subject
    // both repositories have is named with where its files are.
    let alone = self::groups(&derive_subsystems(&layered().snapshot()));
    for (name, kind, members) in &alone {
        let found = groups.iter().find(|group| &group.2 == members);
        let (found, found_kind, _) = found.unwrap_or_else(|| panic!("{name} in {groups:?}"));
        assert_eq!(found_kind, kind);
        assert_eq!(found.split(" (").next(), Some(name.as_str()), "{groups:?}");
    }
    let names: Vec<&str> = groups.iter().map(|group| group.0.as_str()).collect();
    assert!(names.contains(&"Custom domain"), "{names:?}");
    assert!(names.contains(&"Domain"), "{names:?}");
    // Of two groups the same size, both are named with their place.
    for name in ["Alias (app)", "Alias (app/src)"] {
        assert!(names.contains(&name), "{names:?}");
    }
}

const ABUSER: [&str; 4] = [
    "app/abuser.py",
    "app/abuser_utils.py",
    "app/admin/abuser_lookup.py",
    "app/jobs/mark_abuser_job.py",
];
const EVENT: [&str; 6] = [
    "packages/core/src/event-bus/events/order-event.ts",
    "packages/core/src/event-bus/events/login-event.ts",
    "packages/core/src/event-bus/events/refund-event.ts",
    "packages/core/src/event-bus/event-bus.ts",
    "packages/core/src/event-bus/event-bus.module.ts",
    "packages/core/src/event-bus/event-bus-utils.ts",
];
const LEFTOVER: [&str; 4] = [
    "app/dns_utils.py",
    "app/pgp_utils.py",
    "app/jose_utils.py",
    "shell.py",
];

/// The layered concepts beside an abuse concept, an event bus under its own
/// directory and four files that share nothing.
fn mixed() -> Fixture {
    let mut fixture = layered();
    for path in ABUSER {
        fixture.file(path, &["get_abuser", "mark_abuser"]);
    }
    for path in EVENT {
        fixture.file(path, &["publish"]);
    }
    for (path, symbol) in LEFTOVER.iter().zip(["resolve", "encrypt", "sign", "open"]) {
        fixture.file(path, &[symbol]);
    }
    fixture
}

/// The rule's reading of a fixture, and the index of each path in it.
fn read(fixture: &Fixture) -> (RevisionedSnapshot, SubsystemReport) {
    (fixture.snapshot(), SubsystemReport::default())
}

fn files(signals: &Signals<'_>, paths: &[&str]) -> Vec<usize> {
    paths
        .iter()
        .map(|path| {
            signals
                .units
                .iter()
                .position(|unit| unit.path == *path)
                .unwrap()
        })
        .collect()
}

fn regrouped(signals: &Signals<'_>, groups: &[&[&str]]) -> Vec<(String, Vec<String>)> {
    let groups: Vec<Vec<usize>> = groups.iter().map(|paths| files(signals, paths)).collect();
    let groups = regroup(groups, signals);
    let mut found: Vec<(String, Vec<String>)> = named(&groups, signals)
        .into_iter()
        .zip(&groups)
        .map(|((_, name), members)| {
            (
                name.unwrap().title,
                members
                    .iter()
                    .map(|&file| signals.units[file].path.to_owned())
                    .collect(),
            )
        })
        .collect();
    found.sort();
    found
}

fn paths(group: &[&str]) -> Vec<String> {
    let mut paths: Vec<String> = group.iter().map(|p| (*p).to_owned()).collect();
    paths.sort();
    paths
}

/// Every group of the mixed fixture: the regrouping recovers all of them
/// from any partial grouping, since files left out of the given groups that
/// share a word of their names form that group (AC-0970).
fn every_group() -> Vec<(String, Vec<String>)> {
    vec![
        ("Abuser".to_owned(), paths(&ABUSER)),
        ("Alias".to_owned(), paths(&ALIAS)),
        ("Custom domain".to_owned(), paths(&DOMAIN)),
        ("Event bus".to_owned(), paths(&EVENT)),
        ("Mailbox".to_owned(), paths(&MAILBOX)),
    ]
}

// AC-0914: a group named for two subjects becomes one group per subject.
#[test]
fn a_group_of_two_subjects_is_two_groups() {
    let (base, mut report) = read(&mixed());
    let signals = signals(&base, REPO, &mut report).unwrap();
    let both: Vec<&str> = ABUSER.iter().chain(&MAILBOX).copied().collect();
    let name = named(&[files(&signals, &both)], &signals)
        .remove(0)
        .1
        .unwrap();
    assert!(
        name.second.is_some() && name.title.contains(" and "),
        "{}",
        name.title
    );
    assert_eq!(
        regrouped(&signals, &[&both, &ALIAS, &DOMAIN]),
        every_group()
    );
}

// AC-0914: files that share no term are not a group, and are in none.
#[test]
fn files_no_term_holds_together_are_not_a_group() {
    let (base, mut report) = read(&mixed());
    let signals = signals(&base, REPO, &mut report).unwrap();
    assert_eq!(
        regrouped(&signals, &[&LEFTOVER, &ALIAS, &MAILBOX]),
        every_group()
    );
    // The whole rule publishes no group for them either.
    let report = derive_subsystems(&base);
    assert!(
        groups(&report).iter().all(|(_, _, members)| LEFTOVER
            .iter()
            .all(|path| !members.contains(&(*path).to_owned()))),
        "{:?}",
        groups(&report)
    );
}

// AC-0914: two groups with one name are one group, and words that stand
// side by side in a directory name are one phrase.
#[test]
fn groups_with_one_name_are_one_group() {
    let (base, mut report) = read(&mixed());
    let signals = signals(&base, REPO, &mut report).unwrap();
    assert_eq!(
        regrouped(&signals, &[&EVENT[..3], &EVENT[3..], &ALIAS, &MAILBOX]),
        every_group()
    );
}

// AC-0914: a file left in no group joins the group whose subject it carries.
#[test]
fn a_leftover_file_joins_the_group_it_is_named_for() {
    let (base, mut report) = read(&mixed());
    let signals = signals(&base, REPO, &mut report).unwrap();
    assert_eq!(
        regrouped(&signals, &[&ALIAS[..3], &MAILBOX, &DOMAIN]),
        every_group()
    );
}

// AC-0914: the subject is the leading term most files carry, not a rarer
// word that outranks it.
#[test]
fn a_name_is_the_term_most_of_the_group_carries() {
    let (base, mut report) = read(&mixed());
    let signals = signals(&base, REPO, &mut report).unwrap();
    let members = files(&signals, &ABUSER);
    let term = |word: &str, score: f64, cover: usize| Term {
        word: word.into(),
        score,
        cover,
    };
    let name = |terms: &[Term]| title(terms, &members, &signals.units, &signals.words).unwrap();
    let terms = [term("lookup", 0.73, 1), term("abuser", 0.64, 4)];
    assert_eq!(name(&terms).title, "Abuser");
    assert!(coherence(&terms, 4) >= MIN_COHERENCE);
    // A term scoring far below the first does not hold the group together.
    let terms = [term("lookup", 0.73, 1), term("abuser", 0.2, 4)];
    assert!(coherence(&terms, 4) < MIN_COHERENCE);
}

// AC-0970: files in no group that share a word of their own names are that
// subject's group; fewer than three, or sharing no word, are in none.
#[test]
fn leftovers_named_for_one_subject_are_its_group() {
    let (base, mut report) = read(&mixed());
    let signals = signals(&base, REPO, &mut report).unwrap();
    // Only Alias and Mailbox were found: the abuser, custom domain and event
    // bus files form their groups, the unrelated leftovers none.
    assert_eq!(regrouped(&signals, &[&ALIAS, &MAILBOX]), every_group());
    // One alias file left out stays out of a group of its own: it joins Alias.
    assert_eq!(
        regrouped(&signals, &[&ALIAS[..3], &MAILBOX, &ABUSER, &DOMAIN, &EVENT]),
        every_group()
    );
}

const MORE_EVENTS: [&str; 10] = [
    "packages/core/src/event-bus/events/refund-state-transition-event.ts",
    "packages/core/src/event-bus/events/payment-event.ts",
    "packages/core/src/event-bus/events/product-event.ts",
    "packages/core/src/event-bus/events/customer-event.ts",
    "packages/core/src/event-bus/events/channel-event.ts",
    "packages/core/src/event-bus/events/asset-event.ts",
    "packages/core/src/event-bus/events/zone-event.ts",
    "packages/core/src/event-bus/events/role-event.ts",
    "packages/core/src/event-bus/events/facet-event.ts",
    "packages/core/src/event-bus/events/seller-event.ts",
];
const REFUND: [&str; 3] = [
    "src/entity/refund/refund.entity.ts",
    "src/api/resolvers/entity/refund-entity.resolver.ts",
    "src/config/refund/default-refund-process.ts",
];

// AC-1003: a group named for a phrase holds a second subject too, measured
// against the files the first leaves out: the refund entity, resolver and
// process are a small share of a large event bus, yet every file there
// without an event in it is a refund file, so they are their own group.
#[test]
fn a_phrase_group_with_a_second_subject_outside_it_is_two_groups() {
    let mut fixture = mixed();
    for path in MORE_EVENTS {
        fixture.file(path, &["publish"]);
    }
    for path in REFUND {
        fixture.file(path, &["refund"]);
    }
    let (base, mut report) = read(&fixture);
    let signals = signals(&base, REPO, &mut report).unwrap();
    let events: Vec<&str> = EVENT.iter().chain(&MORE_EVENTS).copied().collect();
    let both: Vec<&str> = events.iter().chain(&REFUND).copied().collect();
    // The refund files are fewer than a quarter of the group.
    assert!(REFUND.len() * 4 < both.len());
    let groups: Vec<Vec<usize>> = [&both[..], &ALIAS, &MAILBOX, &ABUSER, &DOMAIN]
        .iter()
        .map(|paths| files(&signals, paths))
        .collect();
    let name = named(&groups, &signals).remove(0).1.unwrap();
    assert_eq!(name.title, "Event bus and refund");
    let mut expected = every_group();
    expected.retain(|(title, _)| title != "Event bus");
    expected.push(("Event bus".to_owned(), paths(&events)));
    expected.push(("Refund".to_owned(), paths(&REFUND)));
    expected.sort();
    assert_eq!(
        regrouped(&signals, &[&both, &ALIAS, &MAILBOX, &ABUSER, &DOMAIN]),
        expected
    );
}
