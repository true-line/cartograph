//! Subsystem candidates (#260): source files grouped by what they are named
//! for and what they use, so documents and topic agents get boundaries that
//! follow the system's concepts instead of its directory layout.
//!
//! A legacy codebase usually files a concept under several technical layers:
//! `api/views/alias.py`, `dashboard/views/alias_transfer.py` and
//! `alias_utils.py` are one subsystem in three directories. The rule reads
//! only base facts and needs no provider:
//!
//! 1. **Units** are files that define a `Symbol` or `Component`, outside
//!    test, vendored and minified trees.
//! 2. **Signals** per file are the words of its name and directories, of the
//!    symbols it defines, of the routes and templates it serves and of the
//!    data entities it writes, that weight shared among the entities so a
//!    file touching many is about none (TF-IDF), the facts it references (calls,
//!    imports, writes; TF-IDF over the targets) and direct calls and imports
//!    between two files.
//! 3. **Shared files**, referenced from many files, are set aside as one
//!    `shared` group: they belong to every concept and would glue the others.
//! 4. **Clusters** are modularity communities of the nearest-neighbour
//!    similarity graph (Louvain, best of a fixed set of visiting orders),
//!    then each file moves to the group it is most similar to until stable.
//! 5. **Names** are the terms most of a group's files carry and few other
//!    groups do.
//!
//! Every step orders its work by file path, so the same base gives the same
//! subsystems under any input order and under any local registry key. A
//! group is a weak semantic candidate (R-INT-1, R-INT-2): it cites every
//! member file and claims nothing about what the code does.
use super::*;
use core_graph::Edge;

/// Deterministic recipe grouping source files into named subsystems.
pub const SUBSYSTEM_RULE: &str = "knowledge.subsystems";
/// Changed units, signals, clustering or naming require a new recipe version.
pub const SUBSYSTEM_RULE_VERSION: &str = "1";
/// Files clustered per run. Similarity is computed per pair of files that
/// share a signal, so the bound keeps one run's work and memory fixed.
pub const MAX_SUBSYSTEM_FILES: usize = 8192;
/// Omission when the base has more than [`MAX_SUBSYSTEM_FILES`] unit files.
pub const SUBSYSTEM_FILE_CAP: &str = "subsystem-file-cap";

/// Smallest group published; smaller ones leave their files ungrouped.
const MIN_FILES: usize = 3;
/// Nearest neighbours each file contributes to the similarity graph.
const NEIGHBOURS: usize = 8;
/// Nearest neighbours kept per file for the final reassignment.
const REFINE_NEIGHBOURS: usize = 32;
/// Similarity below which two files are not neighbours.
const MIN_SIMILARITY: f64 = 0.05;
/// Visiting orders tried; the partition with the highest modularity is kept.
const ORDERS: usize = 16;
/// Modularity resolution: above 1 prefers more, smaller groups. A group that
/// mixes two concepts costs a document more than a concept in two groups.
const RESOLUTION: f64 = 1.3;
/// A file is shared when at least this many files, and this share of all
/// unit files, call or import it.
const SHARED_MIN_FILES: usize = 8;
const SHARED_SHARE: f64 = 0.06;
/// Reassignment divides a file's similarity to a group by the group's size
/// to this power: below 1 so a large coherent group can still attract.
const SIZE_EXPONENT: f64 = 0.7;
/// Evidence spans carried on a group node; every member is still cited.
const MAX_NODE_EVIDENCE: usize = 256;

const W_STEM: f64 = 3.0;
const W_DIRECTORY: f64 = 1.0;
const W_SYMBOL: f64 = 0.25;
const W_SERVED: f64 = 1.0;
const W_ENTITY: f64 = 1.5;
const W_IMPORT: f64 = 0.5;

/// Edges from a file's symbols that say what the file uses.
const USES: &[&str] = &[
    "CALLS",
    "WRITES",
    "READS",
    "RENDERS_TEMPLATE",
    "SUBMITS",
    "NAVIGATES_TO",
    "USES",
    "PUBLISHES",
    "SUBSCRIBES",
    "REFERENCES",
    "EXTENDS",
    "IMPLEMENTS",
    "INJECTS",
];

/// Directory names whose files are not the system's own product code.
const EXCLUDED_DIRECTORIES: &[&str] = &[
    "test",
    "tests",
    "__tests__",
    "__mocks__",
    "__fixtures__",
    "e2e",
    "fixtures",
    "mock",
    "mocks",
    "testing",
    "vendor",
    "vendors",
    "node_modules",
    "third_party",
    "bower_components",
    "migrations",
];

/// Words that name a mechanism, a layer or a language construct, not a
/// subject: they are never a signal and never part of a name.
const STOP: &str = "add admin and anon api app args assets base command commands common config configuration \
    constructor controller controllers core create current dashboard data default delete dto \
    entities entity error false flash for form from get handler handlers has helper helpers \
    impl index init int integer interface internal java job jobs len lib list main make \
    manager method model models module modules name new none not object plugin plugins post \
    redirect remove render request resolver resolvers schema script scripts self service \
    services set shared spec src static str strategy string task tasks template test tests \
    the true type types update url user util utils view views with";

/// Subsystem candidates and how much of the base they cover.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SubsystemReport {
    /// One `Subsystem` node per group and one `MEMBER_OF` edge per member
    /// file, each within the overlay per-fact cap.
    pub facts: Vec<OverlayFact>,
    /// Groups omitted because their node exceeded the per-fact cap; their
    /// membership edges are omitted with them.
    pub oversized_groups: usize,
    /// Unit files considered.
    pub files: usize,
    /// Unit files placed in a concept group.
    pub grouped: usize,
    /// Unit files set aside as shared.
    pub shared: usize,
    /// Named reason nothing was derived, if nothing was.
    pub omission: Option<&'static str>,
}

/// Sparse similarity graph over unit indices; rows are sorted by neighbour.
type Rows = Vec<Vec<(usize, f64)>>;

struct Unit<'a> {
    node: &'a Node,
    path: &'a str,
    evidence: EvidenceRef,
    /// Words of the file name, folded: what a name is built from.
    stem: Vec<String>,
}

/// The lowercase subject words of a name: mechanism words, numbers and
/// fragments of one or two letters are dropped.
fn subject_words(text: &str) -> Vec<String> {
    naming::words(text)
        .into_iter()
        .filter(|w| {
            w.chars().count() > 2
                && !w.chars().all(|c| c.is_ascii_digit())
                && !STOP.split(' ').any(|stop| stop == w)
        })
        .collect()
}

/// A file outside the system's own product code: a test, fixture, vendored
/// or migration tree, a versioned third-party directory (`DataTables-1.10.16`)
/// or a minified, declaration or test file.
fn excluded(path: &str) -> bool {
    let lower = path.to_lowercase();
    let mut segments: Vec<&str> = lower.split('/').collect();
    let file = segments.pop().unwrap_or_default();
    let versioned = |segment: &str| {
        segment.split('-').skip(1).any(|part| {
            let mut numbers = part.split('.');
            let numeric = |n: Option<&str>| {
                n.is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            };
            numeric(numbers.next()) && numeric(numbers.next())
        })
    };
    segments
        .iter()
        .any(|s| EXCLUDED_DIRECTORIES.contains(s) || versioned(s))
        || file.contains(".min.")
        || [".spec.", ".test.", ".e2e-spec.", ".stories.", ".d.ts"]
            .iter()
            .any(|mark| file.contains(mark))
}

fn file_stem(path: &str) -> &str {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.split_once('.')
        .map_or(file, |(stem, _)| if stem.is_empty() { file } else { stem })
}

/// The first source span of a deterministic, source-backed base fact.
fn file_evidence(node: &Node) -> Option<EvidenceRef> {
    let prov: Provenance = serde_json::from_value(node.props.get("prov")?.clone()).ok()?;
    (prov.validate().is_ok()
        && prov.tier == Tier::Deterministic
        && matches!(prov.basis, None | Some(EvidenceBasis::Source)))
    .then_some(())?;
    prov.evidence
        .into_iter()
        .find(|e| !e.repo.is_empty() && !e.path.is_empty() && !e.commit_sha.is_empty())
}

/// TF-IDF weights of one feature family, each file's vector at unit length.
/// A feature in one file, or in more than half of them, separates nothing.
fn weighted<K: Ord + Clone>(features: &[BTreeMap<K, f64>]) -> Vec<BTreeMap<K, f64>> {
    let n = features.len();
    let mut df: BTreeMap<&K, usize> = BTreeMap::new();
    for file in features {
        for (key, weight) in file {
            if *weight > 0.0 {
                *df.entry(key).or_default() += 1;
            }
        }
    }
    features
        .iter()
        .map(|file| {
            let mut vector: BTreeMap<K, f64> = file
                .iter()
                .filter_map(|(key, &tf)| {
                    let df = *df.get(key)?;
                    if tf <= 0.0 || df < 2 || df * 2 > n {
                        return None;
                    }
                    let tf = if tf >= 1.0 { 1.0 + tf.ln() } else { tf };
                    Some((key.clone(), tf * (n as f64 / df as f64).ln()))
                })
                .collect();
            let norm = vector.values().map(|w| w * w).sum::<f64>().sqrt();
            if norm > 0.0 {
                for weight in vector.values_mut() {
                    *weight /= norm;
                }
            }
            vector
        })
        .collect()
}

/// Feature → the files that carry it, in file order.
fn postings<K: Ord + Clone>(vectors: &[BTreeMap<K, f64>]) -> BTreeMap<K, Vec<(usize, f64)>> {
    let mut postings: BTreeMap<K, Vec<(usize, f64)>> = BTreeMap::new();
    for (file, vector) in vectors.iter().enumerate() {
        for (key, &weight) in vector {
            postings
                .entry(key.clone())
                .or_default()
                .push((file, weight));
        }
    }
    postings
}

/// One Louvain level: every node, in `order`, moves to the neighbouring
/// community that most raises modularity, until no node moves. Ties keep the
/// node where it is, then prefer the lower community id.
fn louvain_level(order: &[usize], rows: &Rows, resolution: f64) -> Vec<usize> {
    let n = rows.len();
    let mut community = vec![0usize; n];
    for (position, &node) in order.iter().enumerate() {
        community[node] = position;
    }
    let degree: Vec<f64> = rows
        .iter()
        .map(|row| row.iter().map(|(_, w)| w).sum())
        .collect();
    let total: f64 = degree.iter().sum();
    if total <= 0.0 {
        return community;
    }
    let mut weight_of = vec![0.0; n];
    for node in 0..n {
        weight_of[community[node]] += degree[node];
    }
    for _ in 0..50 {
        let mut moved = false;
        for &node in order {
            let own = community[node];
            let mut links: BTreeMap<usize, f64> = BTreeMap::new();
            for &(other, weight) in &rows[node] {
                if other != node {
                    *links.entry(community[other]).or_default() += weight;
                }
            }
            weight_of[own] -= degree[node];
            let gain =
                |to: usize, link: f64| link - resolution * weight_of[to] * degree[node] / total;
            let mut best = own;
            let mut best_gain = gain(own, links.get(&own).copied().unwrap_or(0.0));
            for (&to, &link) in &links {
                let candidate = gain(to, link);
                if candidate > best_gain + 1e-12 {
                    best = to;
                    best_gain = candidate;
                }
            }
            weight_of[best] += degree[node];
            if best != own {
                community[node] = best;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    community
}

/// Multi-level Louvain: communities found at one level are the nodes of the
/// next, until a level merges nothing. Returns each node's community,
/// numbered from zero.
fn louvain(order: &[usize], rows: &Rows, resolution: f64) -> Vec<usize> {
    let mut part: Vec<usize> = (0..rows.len()).collect();
    let mut level_rows: Rows = rows.clone();
    let mut level_order: Vec<usize> = order.to_vec();
    loop {
        let community = louvain_level(&level_order, &level_rows, resolution);
        let groups: BTreeSet<usize> = community.iter().copied().collect();
        if groups.len() == level_rows.len() {
            break;
        }
        let renumber: BTreeMap<usize, usize> =
            groups.iter().enumerate().map(|(i, &g)| (g, i)).collect();
        for slot in &mut part {
            *slot = renumber[&community[*slot]];
        }
        let mut merged: Vec<BTreeMap<usize, f64>> = vec![BTreeMap::new(); groups.len()];
        for (node, row) in level_rows.iter().enumerate() {
            for &(other, weight) in row {
                *merged[renumber[&community[node]]]
                    .entry(renumber[&community[other]])
                    .or_default() += weight;
            }
        }
        level_rows = merged
            .into_iter()
            .map(|row| row.into_iter().collect())
            .collect();
        level_order = (0..level_rows.len()).collect();
    }
    part
}

fn modularity(rows: &Rows, part: &[usize], resolution: f64) -> f64 {
    let mut inside: BTreeMap<usize, f64> = BTreeMap::new();
    let mut weight_of: BTreeMap<usize, f64> = BTreeMap::new();
    let mut total = 0.0;
    for (node, row) in rows.iter().enumerate() {
        for &(other, weight) in row {
            total += weight;
            *weight_of.entry(part[node]).or_default() += weight;
            if part[other] == part[node] {
                *inside.entry(part[node]).or_default() += weight;
            }
        }
    }
    if total <= 0.0 {
        return 0.0;
    }
    weight_of
        .iter()
        .map(|(group, weight)| {
            inside.get(group).copied().unwrap_or(0.0) / total
                - resolution * (weight / total) * (weight / total)
        })
        .sum()
}

/// Each file moves to the group it is most similar to, similarity summed
/// over its kept neighbours and divided by a power of the group's size,
/// until no file moves. After the first rounds no file joins a group below
/// the published minimum, so fragments dissolve instead of trading files.
fn refine(part: &mut [Option<usize>], near: &Rows) {
    let groups = part.iter().flatten().max().map_or(0, |max| max + 1);
    let mut size = vec![0usize; groups];
    for group in part.iter().flatten() {
        size[*group] += 1;
    }
    for round in 0..30 {
        let mut moved = false;
        for file in 0..part.len() {
            let Some(own) = part[file] else { continue };
            let mut similarity: BTreeMap<usize, f64> = BTreeMap::new();
            for &(other, weight) in &near[file] {
                if let Some(group) = part[other] {
                    *similarity.entry(group).or_default() += weight;
                }
            }
            let mut best: Option<(usize, f64)> = None;
            for (&group, &sum) in &similarity {
                let others = size[group] - usize::from(group == own);
                if others == 0 || (group != own && size[group] < MIN_FILES && round > 2) {
                    continue;
                }
                let score = sum / (others as f64).powf(SIZE_EXPONENT);
                if best.is_none_or(|(_, kept)| score > kept + 1e-12) {
                    best = Some((group, score));
                }
            }
            if let Some((group, _)) = best
                && group != own
            {
                size[own] -= 1;
                size[group] += 1;
                part[file] = Some(group);
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
}

/// Fold a plural onto its singular when the corpus has both (`domains` and
/// `domain`), so `alias` and `status` are left alone.
fn fold(word: &str, vocabulary: &BTreeSet<String>) -> Option<String> {
    let candidates = [
        word.strip_suffix("ies").map(|stem| format!("{stem}y")),
        word.strip_suffix("es").map(str::to_owned),
        word.strip_suffix('s').map(str::to_owned),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|singular| singular.chars().count() > 2 && vocabulary.contains(singular))
}

/// Abbreviations a name spells in capitals, beside the Capability titles' own.
const ACRONYMS: &[&str] = &[
    "aws", "cdn", "cli", "csv", "css", "html", "http", "jwt", "mcp", "pdf", "s3", "sdk", "sms",
    "sql", "tls", "xml",
];

fn spelled(word: &str) -> String {
    if ACRONYMS.contains(&word) {
        word.to_uppercase()
    } else {
        naming::spelled(word).to_owned()
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// A group's ranked terms: the share of its files that carry the term, times
/// the weight the term has in them, times how few other groups carry it.
fn ranked_terms(
    members: &[usize],
    raw: &[BTreeMap<String, f64>],
    vectors: &[BTreeMap<String, f64>],
    groups_with: &BTreeMap<&str, usize>,
    groups: usize,
) -> Vec<(String, f64)> {
    let mut mass: BTreeMap<&str, f64> = BTreeMap::new();
    let mut cover: BTreeMap<&str, usize> = BTreeMap::new();
    for &file in members {
        for (term, weight) in &vectors[file] {
            *mass.entry(term).or_default() += weight;
        }
        for term in raw[file].keys() {
            *cover.entry(term).or_default() += 1;
        }
    }
    let size = members.len() as f64;
    let mut ranked: Vec<(String, f64)> = mass
        .into_iter()
        .map(|(term, mass)| {
            let spread = groups_with.get(term).copied().unwrap_or(1).max(1) as f64;
            let score = (cover.get(term).copied().unwrap_or(0) as f64 / size)
                * (mass / size).sqrt()
                * (1.0 + groups as f64 / spread).ln();
            (term.to_owned(), score)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}

/// The group's name: its first term, joined by the second when that is
/// nearly as strong. Two terms that stand side by side in the members' file
/// names are one phrase in that order (`custom domain`).
fn title(terms: &[(String, f64)], members: &[usize], units: &[Unit<'_>]) -> Option<String> {
    let (first, score) = terms.first()?;
    let second = terms
        .get(1)
        .filter(|(_, other)| *other >= 0.6 * score)
        .map(|(term, _)| term);
    let Some(second) = second else {
        return Some(capitalized(&spelled(first)));
    };
    let (mut either, mut forward, mut backward) = (0usize, 0usize, 0usize);
    for &file in members {
        let stem = &units[file].stem;
        either += usize::from(stem.iter().any(|w| w == first || w == second));
        for pair in stem.windows(2) {
            forward += usize::from(&pair[0] == first && &pair[1] == second);
            backward += usize::from(&pair[0] == second && &pair[1] == first);
        }
    }
    let phrase = either > 0 && (forward + backward) * 10 >= either * 3;
    let (a, b) = if phrase && backward > forward {
        (second, first)
    } else {
        (first, second)
    };
    if spelled(a) == spelled(b) {
        return Some(capitalized(&spelled(a)));
    }
    let joined = if phrase {
        format!("{} {}", spelled(a), spelled(b))
    } else {
        format!("{} and {}", spelled(a), spelled(b))
    };
    Some(capitalized(&joined))
}

/// The directory most of the group's files are in, for a name no term gives
/// or two groups share.
fn common_directory(members: &[usize], units: &[Unit<'_>]) -> String {
    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    for &file in members {
        let directory = units[file]
            .path
            .rsplit_once('/')
            .map_or(".", |(dir, _)| dir);
        *count.entry(directory).or_default() += 1;
    }
    count
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
        .map_or_else(|| ".".to_owned(), |(directory, _)| directory.to_owned())
}

fn provenance(evidence: Vec<EvidenceRef>, content: &[u8]) -> Provenance {
    Provenance::new(
        Tier::Semantic,
        ConfidenceTier::InferredWeak,
        evidence,
        "t2.knowledge.subsystems",
        content,
    )
    .expect("semantic weak is within the confidence ceiling")
    .with_method(ProducingMethod::Rule {
        rule: SUBSYSTEM_RULE.into(),
        version: SUBSYSTEM_RULE_VERSION.into(),
    })
}

/// The node of one group and a membership edge per file, or nothing when
/// the node is past the per-fact cap.
fn group_facts(
    kind: &str,
    name: &str,
    terms: &[String],
    members: &[usize],
    units: &[Unit<'_>],
) -> Option<Vec<OverlayFact>> {
    let ids: Vec<&str> = members.iter().map(|&m| units[m].node.id.as_str()).collect();
    let key = serde_json::to_vec(&(SUBSYSTEM_RULE, SUBSYSTEM_RULE_VERSION, kind, &ids))
        .expect("string tuple serialization");
    let id = format!("subsystem:{kind}:{}", core_prov::content_hash(&key));
    let cites: BTreeSet<BaseRef> = ids
        .iter()
        .map(|id| BaseRef::Node((*id).to_owned()))
        .collect();
    let mut props = serde_json::json!({
        "name": name,
        "kind": kind,
        "terms": terms,
        "files": members.len(),
    });
    let content = serde_json::to_vec(&(&props, &cites)).expect("group content serializes");
    props["prov"] = serde_json::json!(provenance(
        members
            .iter()
            .take(MAX_NODE_EVIDENCE)
            .map(|&m| units[m].evidence.clone())
            .collect(),
        &content,
    ));
    let node = OverlayFact {
        item: OverlayItem::Node(Node {
            id: id.clone(),
            label: "Subsystem".into(),
            props,
        }),
        cites,
    };
    if serde_json::to_string(&node)
        .expect("overlay fact serialization")
        .len()
        > MAX_FACT_BYTES
    {
        return None;
    }
    let mut facts = vec![node];
    for &member in members {
        let file = &units[member].node.id;
        let content = serde_json::to_vec(&(
            SUBSYSTEM_RULE,
            SUBSYSTEM_RULE_VERSION,
            file,
            &id,
            "MEMBER_OF",
        ))
        .expect("membership content serializes");
        facts.push(OverlayFact {
            item: OverlayItem::Edge(Edge {
                src: file.clone(),
                dst: id.clone(),
                label: "MEMBER_OF".into(),
                props: serde_json::json!({
                    "prov": provenance(vec![units[member].evidence.clone()], &content)
                }),
            }),
            cites: BTreeSet::from([BaseRef::Node(file.clone())]),
        });
    }
    Some(facts)
}

/// Derive `Subsystem` candidates and their `MEMBER_OF` edges from the base.
/// The borrowed base is unchanged. A base with too few files to separate
/// yields nothing; one past [`MAX_SUBSYSTEM_FILES`] is a named omission.
pub fn derive_subsystems(base: &RevisionedSnapshot) -> SubsystemReport {
    let mut report = SubsystemReport::default();
    let mut nodes: BTreeMap<&str, Option<&Node>> = BTreeMap::new();
    for node in &base.graph.0 {
        nodes
            .entry(&node.id)
            .and_modify(|seen| *seen = None)
            .or_insert(Some(node));
    }
    let node = |id: &str| nodes.get(id).copied().flatten();

    // A fact's file, and the files that hold tests or define product code.
    let mut file_of: BTreeMap<&str, &str> = BTreeMap::new();
    let mut test_files: BTreeSet<&str> = BTreeSet::new();
    let mut code_files: BTreeSet<&str> = BTreeSet::new();
    for edge in base.graph.1.iter().filter(|e| e.label == "DEFINED_IN") {
        let (Some(member), Some(file)) = (node(&edge.src), node(&edge.dst)) else {
            continue;
        };
        if file.label != "File" {
            continue;
        }
        file_of.insert(&member.id, &file.id);
        match member.label.as_str() {
            "TestCase" => {
                test_files.insert(&file.id);
            }
            "Symbol" | "Component" => {
                code_files.insert(&file.id);
            }
            _ => {}
        }
    }
    let mut units: Vec<Unit<'_>> = code_files
        .iter()
        .filter(|id| !test_files.contains(*id))
        .filter_map(|id| {
            let node = node(id)?;
            let path = node.props["path"].as_str()?;
            (!excluded(path)).then_some(())?;
            Some(Unit {
                node,
                path,
                evidence: file_evidence(node)?,
                stem: Vec::new(),
            })
        })
        .collect();
    // File order is by path, then id: a local source's registry key is
    // random per registration (ADR-0023), and the groups must not follow it.
    units.sort_by(|a, b| a.path.cmp(b.path).then_with(|| a.node.id.cmp(&b.node.id)));
    report.files = units.len();
    if units.len() > MAX_SUBSYSTEM_FILES {
        report.omission = Some(SUBSYSTEM_FILE_CAP);
        return report;
    }
    let n = units.len();
    let index: BTreeMap<&str, usize> = units
        .iter()
        .enumerate()
        .map(|(i, unit)| (unit.node.id.as_str(), i))
        .collect();
    // The unit an edge endpoint belongs to: its file, or itself when a file.
    let unit_of = |id: &str| {
        file_of
            .get(id)
            .and_then(|file| index.get(file))
            .or_else(|| index.get(id))
            .copied()
    };

    let mut words: Vec<BTreeMap<String, f64>> = vec![BTreeMap::new(); n];
    let mut uses: Vec<BTreeMap<String, f64>> = vec![BTreeMap::new(); n];
    let mut direct: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    let mut entities: Vec<BTreeSet<&str>> = vec![BTreeSet::new(); n];
    let add = |words: &mut BTreeMap<String, f64>, text: &str, weight: f64| {
        for word in subject_words(text) {
            *words.entry(word).or_default() += weight;
        }
    };
    for (i, unit) in units.iter().enumerate() {
        add(&mut words[i], file_stem(unit.path), W_STEM);
        if let Some((directories, _)) = unit.path.rsplit_once('/') {
            for directory in directories.split('/') {
                add(&mut words[i], directory, W_DIRECTORY);
            }
        }
    }
    for (member, file) in &file_of {
        if let (Some(&i), Some(member)) = (index.get(file), node(member))
            && matches!(member.label.as_str(), "Symbol" | "Component")
            // An anonymous function's generated name (`webAPI/func@120`,
            // #2007) names nothing; its enclosing function already counts.
            && member.props["func_literal"] != true
        {
            add(
                &mut words[i],
                member.props["name"].as_str().unwrap_or_default(),
                W_SYMBOL,
            );
        }
    }
    for edge in &base.graph.1 {
        let (Some(src), Some(dst)) = (node(&edge.src), node(&edge.dst)) else {
            continue;
        };
        if edge.label == "HANDLES" {
            // The route a file's handler serves names what the file is for.
            if let Some(i) = file_of.get(dst.id.as_str()).and_then(|f| index.get(f)) {
                add(
                    &mut words[*i],
                    src.props["path"].as_str().unwrap_or_default(),
                    W_SERVED,
                );
            }
            continue;
        }
        let Some(from) = unit_of(&src.id) else {
            continue;
        };
        if edge.label == "IMPORTS" {
            if let Some(&to) = index.get(dst.id.as_str())
                && to != from
            {
                *uses[from].entry(format!("import {}", dst.id)).or_default() += 1.0;
                *direct.entry((from, to)).or_default() += W_IMPORT;
            }
            continue;
        }
        if !USES.contains(&edge.label.as_str()) || dst.label == "Gap" {
            continue;
        }
        let to = unit_of(&dst.id);
        if to == Some(from) {
            continue;
        }
        *uses[from].entry(dst.id.clone()).or_default() += 1.0;
        match dst.label.as_str() {
            "DataEntity" => {
                entities[from].insert(dst.props["name"].as_str().unwrap_or_default());
            }
            "Template" => add(
                &mut words[from],
                file_stem(dst.props["path"].as_str().unwrap_or_default()),
                W_SERVED,
            ),
            _ => {}
        }
        if let Some(to) = to {
            *direct.entry((from, to)).or_default() += 1.0;
        }
    }

    // A file that touches one entity is about it; one that touches many (a
    // cleanup, an export) is about none of them in particular.
    for (file, names) in entities.iter().enumerate() {
        let share = W_ENTITY / names.len() as f64;
        for name in names {
            add(&mut words[file], name, share);
        }
    }

    // Plurals fold onto singulars the corpus also uses.
    let vocabulary: BTreeSet<String> = words.iter().flat_map(|w| w.keys().cloned()).collect();
    let folded = |word: &str| fold(word, &vocabulary).unwrap_or_else(|| word.to_owned());
    for file in &mut words {
        let mut merged: BTreeMap<String, f64> = BTreeMap::new();
        for (word, weight) in std::mem::take(file) {
            *merged.entry(folded(&word)).or_default() += weight;
        }
        *file = merged;
    }
    for unit in &mut units {
        unit.stem = subject_words(file_stem(unit.path))
            .iter()
            .map(|word| folded(word))
            .collect();
    }

    // Shared files: called or imported from many files.
    let mut callers = vec![0usize; n];
    for (_, to) in direct.keys() {
        callers[*to] += 1;
    }
    let shared_at = SHARED_MIN_FILES.max((n as f64 * SHARED_SHARE).ceil() as usize);
    let shared: Vec<bool> = callers.iter().map(|&count| count >= shared_at).collect();

    // Similarity rows: word cosine, use cosine and direct coupling.
    let word_vectors = weighted(&words);
    let use_vectors = weighted(&uses);
    let word_postings = postings(&word_vectors);
    let use_postings = postings(&use_vectors);
    let mut coupling: Vec<BTreeMap<usize, f64>> = vec![BTreeMap::new(); n];
    for (&(from, to), &weight) in &direct {
        if shared[from] || shared[to] {
            continue;
        }
        let strength = (weight / 3.0).min(1.0) / (callers[to] as f64).sqrt();
        *coupling[from].entry(to).or_default() += strength;
        *coupling[to].entry(from).or_default() += strength;
    }
    let mut near: Rows = vec![Vec::new(); n];
    let mut row = vec![0.0f64; n];
    for file in 0..n {
        if shared[file] {
            continue;
        }
        row.fill(0.0);
        for (term, weight) in &word_vectors[file] {
            for &(other, other_weight) in &word_postings[term] {
                row[other] += weight * other_weight;
            }
        }
        for (target, weight) in &use_vectors[file] {
            for &(other, other_weight) in &use_postings[target] {
                row[other] += weight * other_weight;
            }
        }
        for (&other, &strength) in &coupling[file] {
            row[other] += strength;
        }
        let mut ranked: Vec<(usize, f64)> = row
            .iter()
            .enumerate()
            .filter(|&(other, &weight)| other != file && !shared[other] && weight > 0.0)
            .map(|(other, &weight)| (other, weight))
            .collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        ranked.truncate(REFINE_NEIGHBOURS);
        near[file] = ranked;
    }
    // Both graphs are symmetric unions of each file's own nearest.
    let symmetric = |keep: usize, floor: f64| -> Rows {
        let mut rows: Vec<BTreeMap<usize, f64>> = vec![BTreeMap::new(); n];
        for (file, ranked) in near.iter().enumerate() {
            for &(other, weight) in ranked.iter().take(keep) {
                if weight > floor {
                    rows[file].insert(other, weight);
                    rows[other].insert(file, weight);
                }
            }
        }
        rows.into_iter()
            .map(|row| row.into_iter().collect())
            .collect()
    };
    let graph = symmetric(NEIGHBOURS, MIN_SIMILARITY);
    let refine_graph = symmetric(REFINE_NEIGHBOURS, 0.0);

    // Best modularity over a fixed set of visiting orders.
    let natural: Vec<usize> = (0..n).collect();
    let mut best: Option<(Vec<usize>, f64)> = None;
    for attempt in 0..ORDERS {
        let mut order = natural.clone();
        if attempt > 0 {
            // A stable sort: files of two repositories at one path keep
            // their order.
            order.sort_by_cached_key(|&file| {
                core_prov::content_hash(format!("{attempt}\u{0}{}", units[file].path).as_bytes())
            });
        }
        let part = louvain(&order, &graph, RESOLUTION);
        let quality = modularity(&graph, &part, RESOLUTION);
        if best.as_ref().is_none_or(|(_, kept)| quality > kept + 1e-12) {
            best = Some((part, quality));
        }
    }
    let Some((communities, _)) = best else {
        return report;
    };
    // Number groups by their first file so ids do not depend on the order.
    let mut renumber: BTreeMap<usize, usize> = BTreeMap::new();
    let mut part: Vec<Option<usize>> = vec![None; n];
    for file in 0..n {
        if !shared[file] {
            let next = renumber.len();
            part[file] = Some(*renumber.entry(communities[file]).or_insert(next));
        }
    }
    refine(&mut part, &refine_graph);

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (file, group) in part.iter().enumerate() {
        if let Some(group) = group {
            groups.entry(*group).or_default().push(file);
        }
    }
    let mut groups: Vec<Vec<usize>> = groups
        .into_values()
        .filter(|members| members.len() >= MIN_FILES)
        .collect();
    groups.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    let mut groups_with: BTreeMap<&str, usize> = BTreeMap::new();
    for members in &groups {
        let terms: BTreeSet<&str> = members
            .iter()
            .flat_map(|&file| word_vectors[file].keys().map(String::as_str))
            .collect();
        for term in terms {
            *groups_with.entry(term).or_default() += 1;
        }
    }
    let mut names: BTreeSet<String> = BTreeSet::new();
    for members in &groups {
        let ranked = ranked_terms(members, &words, &word_vectors, &groups_with, groups.len());
        let directory = common_directory(members, &units);
        let mut name =
            title(&ranked, members, &units).unwrap_or_else(|| format!("Files in {directory}"));
        if names.contains(&name) {
            name = format!("{name} ({directory})");
        }
        let base_name = name.clone();
        let mut suffix = 2;
        while !names.insert(name.clone()) {
            name = format!("{base_name} ({suffix})");
            suffix += 1;
        }
        let terms: Vec<String> = ranked.into_iter().take(6).map(|(term, _)| term).collect();
        match group_facts("cluster", &name, &terms, members, &units) {
            Some(facts) => {
                report.grouped += members.len();
                report.facts.extend(facts);
            }
            None => report.oversized_groups += 1,
        }
    }
    let shared_files: Vec<usize> = (0..n).filter(|&file| shared[file]).collect();
    if !shared_files.is_empty() {
        match group_facts("shared", "Shared code", &[], &shared_files, &units) {
            Some(facts) => {
                report.shared = shared_files.len();
                report.facts.extend(facts);
            }
            None => report.oversized_groups += 1,
        }
    }
    report
}

#[cfg(test)]
#[path = "subsystems_tests.rs"]
mod tests;
