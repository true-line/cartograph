//! Capability candidates from the entry points beyond HTTP (#1997): each
//! process start the entry-point reader recovered, and each schedule file's
//! jobs, is under a capability, so D1 §1.3 lists the work no HTTP request
//! starts.
//!
//! The rule reads only what the readers proved: an `EntryPoint` from
//! `t0.entry-point` with exactly one `ENTRY` into a fact that is not a Gap,
//! and a `Schedule` from `t0.schedule` that is not a scheduled CI workflow.
//! An entry point whose `ENTRY` is a Gap is under no capability: its Gap is
//! in the gap register, which names it (R-INT-4).
//!
//! Operator commands are grouped by the directory, or the manifest or
//! script that declares them (a Node script's subcommands are one group); every other process start is a capability of its own;
//! the jobs of one schedule file, and a Java class's `@Scheduled` methods,
//! are one capability. The title restates the
//! kind the reader recorded and the file's name, never what the code does
//! beyond it.
use super::*;

/// Deterministic recipe grouping entry points and scheduled jobs.
pub const CAPABILITY_ENTRY_POINT_RULE: &str = "knowledge.capability.entry-point";
/// Changed grouping, support or naming semantics require a new recipe version.
pub const CAPABILITY_ENTRY_POINT_RULE_VERSION: &str = "2";

const ENTRY_EXTRACTOR: &str = "t0.entry-point";
const SCHEDULE_EXTRACTOR: &str = "t0.schedule";

/// A file's stem in words: `job_runner.py` reads `job runner`.
fn stem_words(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let stem = if stem == "__main__" {
        path.rsplit('/').nth(1).unwrap_or(stem)
    } else {
        stem
    };
    // A Java class file names its class in CamelCase: `DataLoader` reads
    // `data loader`.
    if path.ends_with(".java") {
        let mut words = String::new();
        let chars: Vec<char> = stem.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            let boundary = i > 0
                && c.is_uppercase()
                && (chars[i - 1].is_lowercase()
                    || chars.get(i + 1).is_some_and(|n| n.is_lowercase()));
            if boundary {
                words.push(' ');
            }
            words.extend(c.to_lowercase());
        }
        return words.replace(['_', '-'], " ").trim().to_string();
    }
    stem.replace(['_', '-'], " ").trim().to_string()
}

/// A class name in words: `com.acme.BootReceiver` reads `boot receiver`.
fn class_words(fqn: &str) -> String {
    let simple = fqn.rsplit('.').next().unwrap_or(fqn);
    stem_words(&format!("{simple}.java"))
}

fn title(kind: &str, file: &str, component: Option<&str>) -> String {
    if let Some(component) = component {
        let words = class_words(component);
        match kind {
            "android-activity" => return format!("Open the {words} screen"),
            "android-service" => return format!("Run the {words} service"),
            "android-receiver" => return format!("Receive broadcasts in {words}"),
            "android-provider" => return format!("Serve content from {words}"),
            "background-job" => return format!("Run {words} in the background"),
            _ => {}
        }
    }
    match kind {
        "smtp-listener" => "Receive mail over SMTP".into(),
        "database-listener" => "Handle database notifications".into(),
        "queue-consumer" => "Consume queue messages".into(),
        "operator-shell" => "Open the operator shell".into(),
        "polling-loop" => format!("Run the {} loop", stem_words(file)),
        "http-server" => "Serve HTTP requests".into(),
        "startup-task" => format!("Run {} at startup", stem_words(file)),
        _ => format!("Run {}", stem_words(file)),
    }
}

fn add_evidence(group: &mut Group, prov: &Provenance) {
    for e in &prov.evidence {
        group
            .evidence
            .entry((
                e.repo.clone(),
                e.path.clone(),
                e.byte_start,
                e.byte_end,
                e.commit_sha.clone(),
            ))
            .or_insert_with(|| e.clone());
    }
}

/// Derive Capability candidates from recovered entry points and schedules.
/// The borrowed base is unchanged.
pub fn derive_entry_point_capabilities(base: &RevisionedSnapshot) -> CapabilityReport {
    // Every derivation under test reads a compacted base, as the knowledge
    // phase reads it (#2215).
    #[cfg(test)]
    let compacted = crate::workbench::compacted_snapshot_for_tests(base);
    #[cfg(test)]
    let base = &compacted;
    let mut nodes: BTreeMap<&str, Option<&Node>> = BTreeMap::new();
    for node in &base.graph.0 {
        nodes
            .entry(&node.id)
            .and_modify(|v| *v = None)
            .or_insert(Some(node));
    }
    let mut entries: BTreeMap<&str, Vec<&core_graph::Edge>> = BTreeMap::new();
    for edge in base.graph.1.iter().filter(|e| e.label == "ENTRY") {
        entries.entry(&edge.src).or_default().push(edge);
    }
    // Group identity → (kind, display file, group).
    let mut groups: BTreeMap<(String, String), (String, String, Group)> = BTreeMap::new();
    // The class an Android component or worker entry names (AC-1110).
    let mut components: BTreeMap<(String, String), String> = BTreeMap::new();
    for node in nodes.values().filter_map(|n| *n) {
        match node.label.as_str() {
            "EntryPoint" => {
                let Some(prov) = support_from(&node.props, ENTRY_EXTRACTOR) else {
                    continue;
                };
                let [edge] = entries.get(node.id.as_str()).map_or(&[][..], Vec::as_slice) else {
                    continue;
                };
                let Some(Some(target)) = nodes.get(edge.dst.as_str()) else {
                    continue;
                };
                if target.label == "Gap" || support_from(&edge.props, ENTRY_EXTRACTOR).is_none() {
                    continue;
                }
                let (Some(kind), Some(file)) =
                    (node.props["kind"].as_str(), node.props["file"].as_str())
                else {
                    continue;
                };
                let key = if kind == "command" {
                    let place = match node.props["how"].as_str() {
                        Some("declared command" | "subcommand") => file.to_owned(),
                        _ => file.rsplit_once('/').map_or("", |(d, _)| d).to_owned(),
                    };
                    ("command".to_owned(), place)
                } else if kind == "scheduled-job" {
                    // A Java class's `@Scheduled` methods are one capability,
                    // as a schedule file's jobs are.
                    ("schedule".to_owned(), file.to_owned())
                } else {
                    ("entry".to_owned(), node.id.clone())
                };
                let place = key.1.clone();
                if let Some(component) = node.props["component"].as_str() {
                    components.insert(key.clone(), component.to_owned());
                }
                let (_, _, group) = groups.entry(key).or_insert_with(|| {
                    let shown = if kind == "command" {
                        place
                    } else {
                        file.to_owned()
                    };
                    let kind = if kind == "scheduled-job" {
                        "schedule"
                    } else {
                        kind
                    };
                    (kind.to_owned(), shown, Group::default())
                });
                group.cites.insert(BaseRef::Node(node.id.clone()));
                add_evidence(group, &prov);
            }
            "Schedule" => {
                let Some(prov) = support_from(&node.props, SCHEDULE_EXTRACTOR) else {
                    continue;
                };
                if node.props["format"] == "github-actions" {
                    continue;
                }
                let Some(file) = node.props["file"].as_str() else {
                    continue;
                };
                let (_, _, group) = groups
                    .entry(("schedule".to_owned(), file.to_owned()))
                    .or_insert_with(|| ("schedule".to_owned(), file.to_owned(), Group::default()));
                group.cites.insert(BaseRef::Node(node.id.clone()));
                add_evidence(group, &prov);
            }
            _ => {}
        }
    }
    let named: Vec<(String, String, (String, String), Group)> = groups
        .into_iter()
        .map(|(key, (kind, file, group))| {
            let title = match kind.as_str() {
                "schedule" => "Run scheduled jobs".to_owned(),
                "command" => "Run operator commands".to_owned(),
                _ => title(&kind, &file, components.get(&key).map(String::as_str)),
            };
            (title, file, key, group)
        })
        .collect();
    let mut by_title: BTreeMap<&str, usize> = BTreeMap::new();
    for (title, ..) in &named {
        *by_title.entry(title).or_default() += 1;
    }
    // Titles that still collide after naming their file (two processes of
    // one Procfile) also name the entry point's key in that file.
    let mut by_file: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for (title, file, ..) in &named {
        *by_file.entry((title, file)).or_default() += 1;
    }
    // Each colliding title names its file in words (#1999): a capability
    // name carries no file path.
    let mut files_of: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (title, file, ..) in &named {
        files_of.entry(title).or_default().push(file);
    }
    let places: BTreeMap<&str, BTreeMap<&str, String>> = files_of
        .into_iter()
        .map(|(title, files)| (title, super::naming::places_apart(files)))
        .collect();
    let titles: Vec<String> = named
        .iter()
        .map(|(title, file, (_, id), _)| {
            let place = &places[title.as_str()][file.as_str()];
            if by_title[title.as_str()] < 2 || place.is_empty() {
                title.clone()
            } else if by_file[&(title.as_str(), file.as_str())] < 2 {
                format!("{title} ({place})")
            } else {
                let entry = id.rsplit_once('#').map_or(id.as_str(), |(_, key)| key);
                let entry = super::naming::spelled_words(entry);
                if place == "process file" {
                    format!("{title} ({entry} process)")
                } else {
                    format!("{title} ({place} {entry})")
                }
            }
        })
        .collect();
    let mut report = CapabilityReport::default();
    for ((_, _, key, group), title) in named.into_iter().zip(titles) {
        let identity = serde_json::to_vec(&(
            CAPABILITY_ENTRY_POINT_RULE,
            CAPABILITY_ENTRY_POINT_RULE_VERSION,
            &key,
        ))
        .expect("string tuple serialization");
        let kind = if key.0 == "schedule" {
            "scheduled-jobs"
        } else {
            "entry-point"
        };
        let fact = capability_fact(
            (
                CAPABILITY_ENTRY_POINT_RULE,
                CAPABILITY_ENTRY_POINT_RULE_VERSION,
            ),
            kind,
            &identity,
            title,
            group,
        );
        retain_sized_fact(&mut report, fact);
    }
    report
}

#[cfg(test)]
mod tests;
