//! Author-declared API reference parsing and deterministic contract-drift
//! recovery (#739, AC-0313).
//!
//! Declared API reference files (Markdown `.md` or OpenAPI 3 `.json`) are parsed
//! deterministically at T0 into `DocumentedRoute` nodes linked by `DOCUMENTED_IN`
//! edges to their `File` node, then compared against the repository's recovered
//! `Endpoint` nodes to emit `Drift` nodes (`kind = "contract-drift"`) linked by
//! `DOCUMENTED_AS` and `SERVED_AS` edges.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Deterministic extractor id for documented API routes and doc-level gaps.
pub const CONTRACT_EXTRACTOR_ID: &str = "t0.api-contract";

/// Deterministic extractor id for API contract drift findings.
pub const CONTRACT_DRIFT_EXTRACTOR_ID: &str = "t0.contract-drift";

/// Canonical Gap reason when an OpenAPI YAML file is declared (#739).
pub const UNSUPPORTED_YAML_REASON: &str =
    "OpenAPI YAML is not supported yet; declare the JSON form";

/// Canonical Gap reason when a declared API doc yields zero routes (#739).
pub const EMPTY_DOC_REASON: &str = "declared API reference has no parsable routes";

/// Canonical reason recorded on `documented-not-served` findings (#739).
pub const DOCUMENTED_NOT_SERVED_REASON: &str = "route absence depends on recovery coverage";

/// HTTP methods recognized by contract extraction (matching `score-api-contract.mjs`).
pub const CONTRACT_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE"];

/// A validated API reference document supplied to contract extraction (#739).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredApiDoc {
    /// Repo-relative slash path (e.g. `docs/api.md`).
    pub path: String,
    /// Optional URL path prefix (starting with `/`) scoping `served-undocumented` checks.
    pub prefix: Option<String>,
    /// Raw UTF-8 file contents.
    pub contents: String,
}

/// A declaration-level validation gap to surface as a `Gap` node in the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractValidationGap {
    /// Declared path (or `.cartograph/api-docs.toml` for config-level failures).
    pub doc_path: String,
    /// Human-readable validation failure reason.
    pub reason: String,
}

/// Graph facts produced by API contract parsing and drift comparison (#739).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct ContractFacts {
    /// `File`, `DocumentedRoute`, `Drift`, and `Gap` nodes.
    pub nodes: Vec<Node>,
    /// `DOCUMENTED_IN`, `DOCUMENTED_AS`, and `SERVED_AS` edges.
    pub edges: Vec<Edge>,
}

fn is_param_segment(segment: &str) -> bool {
    if segment
        .chars()
        .any(|c| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
    {
        return false;
    }
    (segment.starts_with(':') && segment.len() > 1)
        || (segment.starts_with('<') && segment.ends_with('>') && segment.len() > 2)
        || (segment.starts_with('{') && segment.ends_with('}') && segment.len() > 2)
}

/// Normalize an HTTP method and path into the canonical `"METHOD /path"` key
/// defined by `scripts/score-api-contract.mjs`:
/// - trim whitespace;
/// - strip trailing `/` characters;
/// - replace any path segment matching `^(:.+|<.+>|\{.+\})$` with `{}`;
/// - join segments with `/` (or `/` when empty) and prefix uppercase `method`.
pub fn canonical_endpoint(method: &str, path: &str) -> String {
    let trimmed = path.trim().trim_end_matches('/');
    let joined: String = trimmed
        .split('/')
        .map(|seg| if is_param_segment(seg) { "{}" } else { seg })
        .collect::<Vec<_>>()
        .join("/");
    let norm_path = if joined.is_empty() { "/" } else { &joined };
    format!("{} {norm_path}", method.to_ascii_uppercase())
}

/// Split a canonical endpoint key `"METHOD /path"` into `("METHOD", "/path")`.
fn split_canonical(canonical: &str) -> (&str, &str) {
    canonical.split_once(' ').unwrap_or(("", canonical))
}

/// Extract the first path segment of a URL path (e.g. `"/api/v1/users"` -> `"api"`).
fn first_path_segment(path: &str) -> &str {
    path.trim()
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedRouteOccurrence {
    method: String,
    path: String,
    canonical: String,
    byte_start: usize,
    byte_end: usize,
}

fn parse_markdown_heading_line(line: &str) -> Option<(&str, &str)> {
    let after_hashes = line.trim_start_matches('#');
    if after_hashes.len() == line.len() {
        return None;
    }
    let after_ws1 = after_hashes.trim_start_matches(char::is_whitespace);
    if after_ws1.len() == after_hashes.len() {
        return None;
    }
    let method_len = after_ws1
        .bytes()
        .take_while(|b| b.is_ascii_uppercase())
        .count();
    if method_len == 0 {
        return None;
    }
    let method = &after_ws1[..method_len];
    if !CONTRACT_METHODS.contains(&method) {
        return None;
    }
    let after_method = &after_ws1[method_len..];
    let after_ws2 = after_method.trim_start_matches(char::is_whitespace);
    if after_ws2.len() == after_method.len() || !after_ws2.starts_with('/') {
        return None;
    }
    let path_len: usize = after_ws2
        .chars()
        .take_while(|c| !c.is_whitespace())
        .map(char::len_utf8)
        .sum();
    let path = &after_ws2[..path_len];
    Some((method, path))
}

fn parse_markdown_routes(markdown: &str) -> Vec<ParsedRouteOccurrence> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for raw_line in markdown.split('\n') {
        let line_start = offset;
        let line_clean = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let line_end = line_start + line_clean.len();
        offset += raw_line.len() + 1;
        if let Some((method, path)) = parse_markdown_heading_line(raw_line) {
            out.push(ParsedRouteOccurrence {
                method: method.to_string(),
                path: path.to_string(),
                canonical: canonical_endpoint(method, path),
                byte_start: line_start,
                byte_end: line_end,
            });
        }
    }
    out
}

/// Extract the set of canonical endpoint keys from a Markdown API reference,
/// matching `documentedEndpoints` in `scripts/score-api-contract.mjs`.
pub fn documented_endpoints_markdown(markdown: &str) -> BTreeSet<String> {
    parse_markdown_routes(markdown)
        .into_iter()
        .map(|r| r.canonical)
        .collect()
}

/// Locate the byte spans of `<path>` keys inside the top-level `"paths"` object
/// of a validated OpenAPI JSON document. If a key cannot be matched (e.g. non-standard
/// formatting), callers fall back to `(0, raw.len())` citing the whole file.
fn find_openapi_path_key_spans(raw: &str) -> BTreeMap<String, (usize, usize)> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Frame {
        RootObj {
            expect_key: bool,
            next_val_is_paths: bool,
        },
        PathsObj {
            expect_key: bool,
        },
        OtherObj {
            expect_key: bool,
        },
        Array,
    }

    let bytes = raw.as_bytes();
    let mut spans = BTreeMap::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' | b'\r' | b'\n' => {
                i += 1;
            }
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i = (i + 2).min(bytes.len());
                    } else if bytes[i] == b'"' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
                let end = i;
                let mut look = i;
                while look < bytes.len() && matches!(bytes[look], b' ' | b'\t' | b'\r' | b'\n') {
                    look += 1;
                }
                let is_key = look < bytes.len() && bytes[look] == b':';
                if is_key {
                    if let Some(top) = stack.last_mut() {
                        match top {
                            Frame::RootObj {
                                expect_key,
                                next_val_is_paths,
                            } if *expect_key => {
                                *expect_key = false;
                                if let Ok(decoded) =
                                    serde_json::from_str::<String>(&raw[start..end])
                                {
                                    *next_val_is_paths = decoded == "paths";
                                }
                            }
                            Frame::PathsObj { expect_key } if *expect_key => {
                                *expect_key = false;
                                if let Ok(decoded) =
                                    serde_json::from_str::<String>(&raw[start..end])
                                {
                                    spans.entry(decoded).or_insert((start, end));
                                }
                            }
                            Frame::OtherObj { expect_key } if *expect_key => {
                                *expect_key = false;
                            }
                            _ => {}
                        }
                    }
                } else if let Some(Frame::RootObj {
                    next_val_is_paths, ..
                }) = stack.last_mut()
                {
                    *next_val_is_paths = false;
                }
            }
            b'{' => {
                let next_frame = match stack.last_mut() {
                    None => Frame::RootObj {
                        expect_key: true,
                        next_val_is_paths: false,
                    },
                    Some(Frame::RootObj {
                        next_val_is_paths, ..
                    }) if *next_val_is_paths => {
                        *next_val_is_paths = false;
                        Frame::PathsObj { expect_key: true }
                    }
                    _ => Frame::OtherObj { expect_key: true },
                };
                stack.push(next_frame);
                i += 1;
            }
            b'}' => {
                stack.pop();
                i += 1;
            }
            b'[' => {
                if let Some(Frame::RootObj {
                    next_val_is_paths, ..
                }) = stack.last_mut()
                {
                    *next_val_is_paths = false;
                }
                stack.push(Frame::Array);
                i += 1;
            }
            b']' => {
                stack.pop();
                i += 1;
            }
            b',' => {
                if let Some(top) = stack.last_mut() {
                    match top {
                        Frame::RootObj {
                            expect_key,
                            next_val_is_paths,
                        } => {
                            *expect_key = true;
                            *next_val_is_paths = false;
                        }
                        Frame::PathsObj { expect_key } | Frame::OtherObj { expect_key } => {
                            *expect_key = true;
                        }
                        Frame::Array => {}
                    }
                }
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    spans
}

fn parse_openapi_json_routes(
    raw: &str,
    doc_path: &str,
) -> Result<Vec<ParsedRouteOccurrence>, String> {
    let root: serde_json::Value = serde_json::from_str(raw)
        .map_err(|err| format!("invalid OpenAPI JSON in \"{doc_path}\": {err}"))?;
    let Some(obj) = root.as_object() else {
        return Err(format!(
            "invalid OpenAPI JSON in \"{doc_path}\": root must be a JSON object"
        ));
    };
    let Some(version) = obj.get("openapi").and_then(serde_json::Value::as_str) else {
        return Err(format!(
            "OpenAPI JSON \"{doc_path}\" must declare an \"openapi\" string version starting with \"3.\""
        ));
    };
    if !version.starts_with("3.") {
        return Err(format!(
            "OpenAPI JSON \"{doc_path}\" declares unsupported version \"{version}\" (expected \"3.x\")"
        ));
    }

    let key_spans = find_openapi_path_key_spans(raw);
    let mut out = Vec::new();
    if let Some(paths) = obj.get("paths").and_then(serde_json::Value::as_object) {
        for (raw_path, item) in paths {
            let Some(methods_obj) = item.as_object() else {
                continue;
            };
            // Cite the `paths.<path>` key token's byte span in the JSON file;
            // fall back to citing the whole file `(0..raw.len())` if not found.
            let (byte_start, byte_end) = key_spans.get(raw_path).copied().unwrap_or((0, raw.len()));
            for (raw_method, _op) in methods_obj {
                let upper = raw_method.to_ascii_uppercase();
                if CONTRACT_METHODS.contains(&upper.as_str()) {
                    out.push(ParsedRouteOccurrence {
                        canonical: canonical_endpoint(&upper, raw_path),
                        method: upper,
                        path: raw_path.clone(),
                        byte_start,
                        byte_end,
                    });
                }
            }
        }
    }
    Ok(out)
}

fn evidence(repo: &str, path: &str, commit: &str, start: usize, end: usize) -> EvidenceRef {
    EvidenceRef {
        repo: repo.into(),
        path: path.into(),
        byte_start: start as u64,
        byte_end: end as u64,
        commit_sha: commit.into(),
    }
}

fn typed_provenance(value: &serde_json::Value) -> Option<Provenance> {
    Provenance::deserialize(&*crate::stored_prov(value))
        .ok()
        .filter(|provenance| provenance.validate().is_ok())
}

fn merge_evidence<'a>(provenances: impl IntoIterator<Item = &'a Provenance>) -> Vec<EvidenceRef> {
    let mut map = BTreeMap::new();
    for prov in provenances {
        for ev in &prov.evidence {
            let key = (
                ev.repo.clone(),
                ev.path.clone(),
                ev.byte_start,
                ev.byte_end,
                ev.commit_sha.clone(),
            );
            map.entry(key).or_insert_with(|| ev.clone());
        }
    }
    map.into_values().collect()
}

fn ensure_file_node(
    facts: &mut ContractFacts,
    is_candidate: &dyn Fn(&str) -> bool,
    emitted_files: &mut BTreeSet<String>,
    repo: &str,
    commit: &str,
    doc_path: &str,
    byte_len: usize,
) -> String {
    let file_id = format!("file:{repo}@{doc_path}");
    if is_candidate(file_id.as_str()) || !emitted_files.insert(file_id.clone()) {
        return file_id;
    }
    let ev = evidence(repo, doc_path, commit, 0, byte_len);
    let prov = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![ev],
        CONTRACT_EXTRACTOR_ID,
        file_id.as_bytes(),
    )
    .expect("Confirmed is within the deterministic confidence ceiling")
    .with_method(crate::spec_rule(CONTRACT_EXTRACTOR_ID));
    facts.nodes.push(Node {
        id: file_id.clone(),
        label: "File".into(),
        props: serde_json::json!({
            "path": doc_path,
            "prov": prov,
        }),
    });
    file_id
}

fn emit_doc_gap(
    facts: &mut ContractFacts,
    repo: &str,
    commit: &str,
    doc_path: &str,
    byte_len: usize,
    reason: &str,
    file_id: Option<&str>,
) {
    let hash = blake3::hash(format!("contract-gap:{repo}:{doc_path}:{reason}").as_bytes()).to_hex();
    let gap_id = format!("gap:contract:{repo}@{doc_path}#{}", &hash.as_str()[..12]);
    let ev = evidence(repo, doc_path, commit, 0, byte_len);
    let prov = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Gap,
        vec![ev.clone()],
        CONTRACT_EXTRACTOR_ID,
        serde_json::to_vec(&(&gap_id, doc_path, reason))
            .expect("gap identity serializes")
            .as_slice(),
    )
    .expect("Gap is within the deterministic confidence ceiling")
    .with_method(crate::spec_rule(CONTRACT_EXTRACTOR_ID));

    facts.nodes.push(Node {
        id: gap_id.clone(),
        label: "Gap".into(),
        props: serde_json::json!({
            "kind": "contract-doc-gap",
            "doc_path": doc_path,
            "reason": reason,
            "attempted_tiers": ["T0"],
            "prov": prov,
        }),
    });

    if let Some(target_file_id) = file_id {
        let edge_prov = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![ev],
            CONTRACT_EXTRACTOR_ID,
            format!("{gap_id} DOCUMENTED_IN {target_file_id}").as_bytes(),
        )
        .expect("Confirmed is within the deterministic confidence ceiling")
        .with_method(crate::spec_rule(CONTRACT_EXTRACTOR_ID));
        facts.edges.push(Edge {
            src: gap_id,
            dst: target_file_id.to_string(),
            label: "DOCUMENTED_IN".into(),
            props: serde_json::json!({ "prov": edge_prov }),
        });
    }
}

struct ParsedDocScope {
    doc_path: String,
    prefix: Option<String>,
    first_segments: BTreeSet<String>,
    file_evidence: EvidenceRef,
}

impl ParsedDocScope {
    fn covers_path(&self, raw_path: &str, canonical_path: &str) -> bool {
        if let Some(prefix) = &self.prefix {
            raw_path.starts_with(prefix.as_str()) || canonical_path.starts_with(prefix.as_str())
        } else {
            let raw_seg = first_path_segment(raw_path);
            let canon_seg = first_path_segment(canonical_path);
            (!raw_seg.is_empty() && self.first_segments.contains(raw_seg))
                || (!canon_seg.is_empty() && self.first_segments.contains(canon_seg))
                || (raw_seg.is_empty() && canon_seg.is_empty() && self.first_segments.contains(""))
        }
    }
}

/// Extract `DocumentedRoute`, contract `Drift`, and contract `Gap` facts for
/// one repository from its validated API doc declarations and recovered
/// candidate nodes (#739, AC-0313).
pub fn extract_contract_facts(
    repo: &str,
    commit: &str,
    docs: &[DeclaredApiDoc],
    validation_gaps: &[ContractValidationGap],
    candidates: &[Node],
) -> ContractFacts {
    if docs.is_empty() && validation_gaps.is_empty() {
        return ContractFacts::default();
    }
    let ids: BTreeSet<&str> = candidates.iter().map(|node| node.id.as_str()).collect();
    extract_contract_facts_among(repo, commit, docs, validation_gaps, candidates, |id| {
        ids.contains(id)
    })
}

/// [`extract_contract_facts`] over the recovered `endpoints` (any other node
/// in the slice is ignored) and an id membership test for every recovered
/// fact, so a caller holding packed facts decodes only Endpoint nodes (#1574).
pub fn extract_contract_facts_among(
    repo: &str,
    commit: &str,
    docs: &[DeclaredApiDoc],
    validation_gaps: &[ContractValidationGap],
    endpoints: &[Node],
    is_candidate: impl Fn(&str) -> bool,
) -> ContractFacts {
    let mut facts = ContractFacts::default();
    if docs.is_empty() && validation_gaps.is_empty() {
        return facts;
    }

    let mut emitted_files = BTreeSet::new();

    for vgap in validation_gaps {
        emit_doc_gap(
            &mut facts,
            repo,
            commit,
            &vgap.doc_path,
            0,
            &vgap.reason,
            None,
        );
    }

    // Check recovered Endpoint nodes once so unusable endpoints can be reported
    // in Gap text whenever API docs are declared.
    let mut recovered_by_key: BTreeMap<String, Vec<(&Node, String, String)>> = BTreeMap::new();
    let mut unusable_endpoints = 0usize;
    // HTTP API docs describe HTTP routes; GraphQL operations (#968) are
    // neither matched against them nor counted as unusable.
    for node in endpoints
        .iter()
        .filter(|n| n.label == "Endpoint" && n.props["protocol"] != "graphql")
    {
        let Some(method) = node.props["method"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
        else {
            unusable_endpoints += 1;
            continue;
        };
        let Some(path) = node.props["path"].as_str().filter(|s| !s.trim().is_empty()) else {
            unusable_endpoints += 1;
            continue;
        };
        let canonical = canonical_endpoint(method, path);
        let (_, canonical_path) = split_canonical(&canonical);
        recovered_by_key
            .entry(canonical.clone())
            .or_default()
            .push((node, path.trim().to_string(), canonical_path.to_string()));
    }
    for list in recovered_by_key.values_mut() {
        list.sort_by(|(a, _, _), (b, _, _)| a.id.cmp(&b.id));
    }

    if unusable_endpoints > 0 {
        let gap_doc_path = docs
            .first()
            .map(|d| d.path.as_str())
            .or_else(|| validation_gaps.first().map(|g| g.doc_path.as_str()))
            .unwrap_or(".cartograph/api-docs.toml");
        emit_doc_gap(
            &mut facts,
            repo,
            commit,
            gap_doc_path,
            0,
            &format!(
                "{unusable_endpoints} recovered Endpoint node(s) without method and path props were ignored"
            ),
            None,
        );
    }

    let mut parsed_scopes: Vec<ParsedDocScope> = Vec::new();
    let mut doc_route_nodes_by_key: BTreeMap<String, Vec<Node>> = BTreeMap::new();

    for doc in docs {
        let file_id = ensure_file_node(
            &mut facts,
            &is_candidate,
            &mut emitted_files,
            repo,
            commit,
            &doc.path,
            doc.contents.len(),
        );
        let ext = Path::new(&doc.path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());

        let occurrences = match ext.as_deref() {
            Some("md") => parse_markdown_routes(&doc.contents),
            Some("json") => match parse_openapi_json_routes(&doc.contents, &doc.path) {
                Ok(routes) => routes,
                Err(reason) => {
                    emit_doc_gap(
                        &mut facts,
                        repo,
                        commit,
                        &doc.path,
                        doc.contents.len(),
                        &reason,
                        Some(&file_id),
                    );
                    continue;
                }
            },
            Some("yaml" | "yml") => {
                emit_doc_gap(
                    &mut facts,
                    repo,
                    commit,
                    &doc.path,
                    doc.contents.len(),
                    UNSUPPORTED_YAML_REASON,
                    Some(&file_id),
                );
                continue;
            }
            _ => {
                emit_doc_gap(
                    &mut facts,
                    repo,
                    commit,
                    &doc.path,
                    doc.contents.len(),
                    &format!(
                        "unsupported API reference format for \"{}\"; declare Markdown (.md) or OpenAPI 3 JSON (.json)",
                        doc.path
                    ),
                    Some(&file_id),
                );
                continue;
            }
        };

        if occurrences.is_empty() {
            let reason = if unusable_endpoints > 0 {
                format!(
                    "{EMPTY_DOC_REASON} ({unusable_endpoints} unusable Endpoint node(s) ignored)"
                )
            } else {
                EMPTY_DOC_REASON.to_string()
            };
            emit_doc_gap(
                &mut facts,
                repo,
                commit,
                &doc.path,
                doc.contents.len(),
                &reason,
                Some(&file_id),
            );
            continue;
        }

        // Group duplicate headings by canonical key within the same doc so one
        // deterministic DocumentedRoute node cites every occurrence.
        #[derive(Default)]
        struct RouteGroup {
            method: String,
            path: String,
            spans: BTreeSet<(usize, usize)>,
        }
        let mut by_canonical: BTreeMap<String, RouteGroup> = BTreeMap::new();
        let mut first_segments = BTreeSet::new();

        for occ in occurrences {
            let (_, canon_path) = split_canonical(&occ.canonical);
            first_segments.insert(first_path_segment(canon_path).to_string());
            let entry = by_canonical
                .entry(occ.canonical.clone())
                .or_insert_with(|| RouteGroup {
                    method: occ.method.clone(),
                    path: occ.path.clone(),
                    spans: BTreeSet::new(),
                });
            entry.spans.insert((occ.byte_start, occ.byte_end));
        }

        for (canonical, group) in by_canonical {
            let route_id = format!("doc-route:{repo}@{}#{canonical}", doc.path);
            let ev_list: Vec<EvidenceRef> = group
                .spans
                .iter()
                .map(|&(start, end)| evidence(repo, &doc.path, commit, start, end))
                .collect();
            let prov = Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                ev_list.clone(),
                CONTRACT_EXTRACTOR_ID,
                serde_json::to_vec(&(&route_id, &group.method, &group.path, &canonical, &doc.path))
                    .expect("DocumentedRoute identity serializes")
                    .as_slice(),
            )
            .expect("Confirmed is within the deterministic confidence ceiling")
            .with_method(crate::spec_rule(CONTRACT_EXTRACTOR_ID));

            let route_node = Node {
                id: route_id.clone(),
                label: "DocumentedRoute".into(),
                props: serde_json::json!({
                    "method": group.method,
                    "path": group.path,
                    "canonical": canonical,
                    "doc_path": doc.path,
                    "prov": prov,
                }),
            };
            facts.nodes.push(route_node.clone());
            doc_route_nodes_by_key
                .entry(canonical)
                .or_default()
                .push(route_node);

            let edge_prov = Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                ev_list,
                CONTRACT_EXTRACTOR_ID,
                format!("{route_id} DOCUMENTED_IN {file_id}").as_bytes(),
            )
            .expect("Confirmed is within the deterministic confidence ceiling")
            .with_method(crate::spec_rule(CONTRACT_EXTRACTOR_ID));
            facts.edges.push(Edge {
                src: route_id,
                dst: file_id.clone(),
                label: "DOCUMENTED_IN".into(),
                props: serde_json::json!({ "prov": edge_prov }),
            });
        }

        parsed_scopes.push(ParsedDocScope {
            doc_path: doc.path.clone(),
            prefix: doc.prefix.clone(),
            first_segments,
            file_evidence: evidence(repo, &doc.path, commit, 0, doc.contents.len()),
        });
    }

    if parsed_scopes.is_empty() {
        facts.nodes.sort_by(|a, b| a.id.cmp(&b.id));
        facts
            .edges
            .sort_by(|a, b| (&a.src, &a.dst, &a.label).cmp(&(&b.src, &b.dst, &b.label)));
        return facts;
    }

    // Group documented and recovered routes by canonical path to detect
    // disjoint method sets (`method-mismatch`).
    let mut doc_methods_by_path: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for key in doc_route_nodes_by_key.keys() {
        let (method, canon_path) = split_canonical(key);
        doc_methods_by_path
            .entry(canon_path.to_string())
            .or_default()
            .insert(method.to_string());
    }

    let mut served_methods_by_path: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for key in recovered_by_key.keys() {
        let (method, canon_path) = split_canonical(key);
        served_methods_by_path
            .entry(canon_path.to_string())
            .or_default()
            .insert(method.to_string());
    }

    let mut mismatch_paths: BTreeSet<String> = BTreeSet::new();
    for (canon_path, doc_methods) in &doc_methods_by_path {
        if let Some(served_methods) = served_methods_by_path.get(canon_path)
            && !doc_methods.is_empty()
            && !served_methods.is_empty()
            && doc_methods.is_disjoint(served_methods)
        {
            mismatch_paths.insert(canon_path.clone());
            emit_method_mismatch_drift(
                &mut facts,
                repo,
                canon_path,
                doc_methods,
                served_methods,
                &doc_route_nodes_by_key,
                &recovered_by_key,
            );
        }
    }

    // 1. documented-not-served (InferredStrong)
    for (key, doc_nodes) in &doc_route_nodes_by_key {
        let (_, canon_path) = split_canonical(key);
        if mismatch_paths.contains(canon_path) || recovered_by_key.contains_key(key) {
            continue;
        }
        emit_documented_not_served_drift(&mut facts, repo, key, doc_nodes);
    }

    // 2. served-undocumented (Confirmed, scoped by declaration prefix or first segment)
    for (key, endpoints) in &recovered_by_key {
        let (_, canon_path) = split_canonical(key);
        if mismatch_paths.contains(canon_path) || doc_route_nodes_by_key.contains_key(key) {
            continue;
        }
        let matching_scopes: Vec<&ParsedDocScope> = parsed_scopes
            .iter()
            .filter(|scope| {
                endpoints
                    .iter()
                    .any(|(_, raw_path, c_path)| scope.covers_path(raw_path, c_path))
            })
            .collect();
        if matching_scopes.is_empty() {
            continue;
        }
        let ep_nodes: Vec<&Node> = endpoints.iter().map(|(n, _, _)| *n).collect();
        emit_served_undocumented_drift(&mut facts, repo, key, &ep_nodes, &matching_scopes);
    }

    facts.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    facts
        .edges
        .sort_by(|a, b| (&a.src, &a.dst, &a.label).cmp(&(&b.src, &b.dst, &b.label)));
    facts
}

fn format_documented_summary(doc_nodes: &[Node]) -> String {
    let mut parts = Vec::new();
    for node in doc_nodes {
        let method = node.props["method"].as_str().unwrap_or("");
        let path = node.props["path"].as_str().unwrap_or("");
        let doc_path = node.props["doc_path"].as_str().unwrap_or("");
        parts.push(format!("{method} {path} ({doc_path})"));
    }
    parts.join(", ")
}

fn format_served_summary(ep_nodes: &[&Node]) -> String {
    let mut parts = Vec::new();
    for node in ep_nodes {
        let method = node.props["method"].as_str().unwrap_or("");
        let path = node.props["path"].as_str().unwrap_or("");
        parts.push(format!("{method} {path} ({})", node.id));
    }
    parts.join(", ")
}

fn attach_reason_to_prov(prov: &Provenance, reason: &str) -> serde_json::Value {
    let mut val = serde_json::to_value(prov).expect("provenance serializes");
    if let Some(obj) = val.as_object_mut() {
        obj.insert("reason".into(), serde_json::Value::String(reason.into()));
    }
    val
}

fn emit_documented_not_served_drift(
    facts: &mut ContractFacts,
    repo: &str,
    key: &str,
    doc_nodes: &[Node],
) {
    let drift_id = format!("drift:contract:{repo}@documented-not-served:{key}");
    let doc_provs: Vec<Provenance> = doc_nodes
        .iter()
        .filter_map(|n| typed_provenance(&n.props))
        .collect();
    let cited = merge_evidence(&doc_provs);
    let prov = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::InferredStrong,
        cited,
        CONTRACT_DRIFT_EXTRACTOR_ID,
        serde_json::to_vec(&("documented-not-served", repo, key))
            .expect("drift identity serializes")
            .as_slice(),
    )
    .expect("InferredStrong is within the deterministic confidence ceiling")
    .with_method(crate::spec_rule(CONTRACT_DRIFT_EXTRACTOR_ID));

    let documented_summary = format_documented_summary(doc_nodes);
    let prov_json = attach_reason_to_prov(&prov, DOCUMENTED_NOT_SERVED_REASON);

    facts.nodes.push(Node {
        id: drift_id.clone(),
        label: "Drift".into(),
        props: serde_json::json!({
            "kind": "contract-drift",
            "drift_kind": "documented-not-served",
            "key": key,
            "canonical": key,
            "title": format!("documented-not-served: {key}"),
            "documented": documented_summary,
            "served": "—",
            "reason": DOCUMENTED_NOT_SERVED_REASON,
            "prov": prov_json,
        }),
    });

    for doc_node in doc_nodes {
        let edge_ev = typed_provenance(&doc_node.props)
            .map(|p| p.evidence)
            .unwrap_or_default();
        let edge_prov = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::InferredStrong,
            edge_ev,
            CONTRACT_DRIFT_EXTRACTOR_ID,
            format!("{drift_id} DOCUMENTED_AS {}", doc_node.id).as_bytes(),
        )
        .expect("InferredStrong is within the deterministic confidence ceiling")
        .with_method(crate::spec_rule(CONTRACT_DRIFT_EXTRACTOR_ID));
        facts.edges.push(Edge {
            src: drift_id.clone(),
            dst: doc_node.id.clone(),
            label: "DOCUMENTED_AS".into(),
            props: serde_json::json!({
                "reason": DOCUMENTED_NOT_SERVED_REASON,
                "prov": attach_reason_to_prov(&edge_prov, DOCUMENTED_NOT_SERVED_REASON),
            }),
        });
    }
}

/// Confidence a recovered endpoint contributes to a served-side finding. An
/// endpoint without typed provenance is never treated as Confirmed.
fn endpoint_confidence(node: &Node) -> ConfidenceTier {
    typed_provenance(&node.props)
        .map(|prov| prov.confidence_tier)
        .unwrap_or(ConfidenceTier::InferredWeak)
}

/// Weakest of the cited confidences, so a finding never claims more certainty
/// than the recovered endpoints it depends on (Confirmed when none are cited).
fn weakest_confidence(tiers: impl IntoIterator<Item = ConfidenceTier>) -> ConfidenceTier {
    let rank = |tier: ConfidenceTier| match tier {
        ConfidenceTier::Confirmed => 0,
        ConfidenceTier::InferredStrong => 1,
        ConfidenceTier::InferredWeak => 2,
        ConfidenceTier::Gap => 3,
    };
    tiers
        .into_iter()
        .max_by_key(|tier| rank(*tier))
        .unwrap_or(ConfidenceTier::Confirmed)
}

fn emit_served_undocumented_drift(
    facts: &mut ContractFacts,
    repo: &str,
    key: &str,
    ep_nodes: &[&Node],
    scopes: &[&ParsedDocScope],
) {
    let drift_id = format!("drift:contract:{repo}@served-undocumented:{key}");
    let ep_provs: Vec<Provenance> = ep_nodes
        .iter()
        .filter_map(|n| typed_provenance(&n.props))
        .collect();
    let mut cited = merge_evidence(&ep_provs);
    for scope in scopes {
        if !cited.contains(&scope.file_evidence) {
            cited.push(scope.file_evidence.clone());
        }
    }
    let prov = Provenance::new(
        Tier::Deterministic,
        weakest_confidence(ep_nodes.iter().map(|node| endpoint_confidence(node))),
        cited,
        CONTRACT_DRIFT_EXTRACTOR_ID,
        serde_json::to_vec(&("served-undocumented", repo, key))
            .expect("drift identity serializes")
            .as_slice(),
    )
    .expect("endpoint confidence is within the deterministic confidence ceiling")
    .with_method(crate::spec_rule(CONTRACT_DRIFT_EXTRACTOR_ID));

    let served_summary = format_served_summary(ep_nodes);
    let doc_names: Vec<&str> = scopes.iter().map(|s| s.doc_path.as_str()).collect();
    let documented_summary = format!("absent in {}", doc_names.join(", "));

    facts.nodes.push(Node {
        id: drift_id.clone(),
        label: "Drift".into(),
        props: serde_json::json!({
            "kind": "contract-drift",
            "drift_kind": "served-undocumented",
            "key": key,
            "canonical": key,
            "title": format!("served-undocumented: {key}"),
            "documented": documented_summary,
            "served": served_summary,
            "prov": prov,
        }),
    });

    for ep_node in ep_nodes {
        let mut edge_ev = typed_provenance(&ep_node.props)
            .map(|p| p.evidence)
            .unwrap_or_default();
        for scope in scopes {
            if !edge_ev.contains(&scope.file_evidence) {
                edge_ev.push(scope.file_evidence.clone());
            }
        }
        let edge_prov = Provenance::new(
            Tier::Deterministic,
            endpoint_confidence(ep_node),
            edge_ev,
            CONTRACT_DRIFT_EXTRACTOR_ID,
            format!("{drift_id} SERVED_AS {}", ep_node.id).as_bytes(),
        )
        .expect("endpoint confidence is within the deterministic confidence ceiling")
        .with_method(crate::spec_rule(CONTRACT_DRIFT_EXTRACTOR_ID));
        facts.edges.push(Edge {
            src: drift_id.clone(),
            dst: ep_node.id.clone(),
            label: "SERVED_AS".into(),
            props: serde_json::json!({ "prov": edge_prov }),
        });
    }
}

fn emit_method_mismatch_drift(
    facts: &mut ContractFacts,
    repo: &str,
    canon_path: &str,
    doc_methods: &BTreeSet<String>,
    served_methods: &BTreeSet<String>,
    doc_route_nodes_by_key: &BTreeMap<String, Vec<Node>>,
    recovered_by_key: &BTreeMap<String, Vec<(&Node, String, String)>>,
) {
    let drift_id = format!("drift:contract:{repo}@method-mismatch:{canon_path}");
    let mut matched_doc_nodes: Vec<Node> = Vec::new();
    for m in doc_methods {
        let key = format!("{m} {canon_path}");
        if let Some(nodes) = doc_route_nodes_by_key.get(&key) {
            matched_doc_nodes.extend(nodes.iter().cloned());
        }
    }
    let mut matched_ep_nodes: Vec<&Node> = Vec::new();
    for m in served_methods {
        let key = format!("{m} {canon_path}");
        if let Some(entries) = recovered_by_key.get(&key) {
            matched_ep_nodes.extend(entries.iter().map(|(n, _, _)| *n));
        }
    }

    let mut all_provs: Vec<Provenance> = matched_doc_nodes
        .iter()
        .filter_map(|n| typed_provenance(&n.props))
        .collect();
    all_provs.extend(
        matched_ep_nodes
            .iter()
            .filter_map(|n| typed_provenance(&n.props)),
    );
    let cited = merge_evidence(&all_provs);

    let prov = Provenance::new(
        Tier::Deterministic,
        weakest_confidence(
            matched_ep_nodes
                .iter()
                .map(|node| endpoint_confidence(node)),
        ),
        cited,
        CONTRACT_DRIFT_EXTRACTOR_ID,
        serde_json::to_vec(&(
            "method-mismatch",
            repo,
            canon_path,
            doc_methods,
            served_methods,
        ))
        .expect("drift identity serializes")
        .as_slice(),
    )
    .expect("endpoint confidence is within the deterministic confidence ceiling")
    .with_method(crate::spec_rule(CONTRACT_DRIFT_EXTRACTOR_ID));

    let documented_summary = format_documented_summary(&matched_doc_nodes);
    let served_summary = format_served_summary(&matched_ep_nodes);

    facts.nodes.push(Node {
        id: drift_id.clone(),
        label: "Drift".into(),
        props: serde_json::json!({
            "kind": "contract-drift",
            "drift_kind": "method-mismatch",
            "key": canon_path,
            "canonical": canon_path,
            "title": format!("method-mismatch: {canon_path}"),
            "documented": documented_summary,
            "served": served_summary,
            "prov": prov,
        }),
    });

    for doc_node in &matched_doc_nodes {
        let edge_ev = typed_provenance(&doc_node.props)
            .map(|p| p.evidence)
            .unwrap_or_default();
        let edge_prov = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            edge_ev,
            CONTRACT_DRIFT_EXTRACTOR_ID,
            format!("{drift_id} DOCUMENTED_AS {}", doc_node.id).as_bytes(),
        )
        .expect("Confirmed is within the deterministic confidence ceiling")
        .with_method(crate::spec_rule(CONTRACT_DRIFT_EXTRACTOR_ID));
        facts.edges.push(Edge {
            src: drift_id.clone(),
            dst: doc_node.id.clone(),
            label: "DOCUMENTED_AS".into(),
            props: serde_json::json!({ "prov": edge_prov }),
        });
    }

    for ep_node in &matched_ep_nodes {
        let edge_ev = typed_provenance(&ep_node.props)
            .map(|p| p.evidence)
            .unwrap_or_default();
        let edge_prov = Provenance::new(
            Tier::Deterministic,
            endpoint_confidence(ep_node),
            edge_ev,
            CONTRACT_DRIFT_EXTRACTOR_ID,
            format!("{drift_id} SERVED_AS {}", ep_node.id).as_bytes(),
        )
        .expect("endpoint confidence is within the deterministic confidence ceiling")
        .with_method(crate::spec_rule(CONTRACT_DRIFT_EXTRACTOR_ID));
        facts.edges.push(Edge {
            src: drift_id.clone(),
            dst: ep_node.id.clone(),
            label: "SERVED_AS".into(),
            props: serde_json::json!({ "prov": edge_prov }),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExportMode, compile_spec, is_drift_edge, is_drift_node, is_gap_node};

    fn endpoint_node(repo: &str, method: &str, path: &str, file: &str) -> Node {
        let id = format!("ep:{repo}@{method}:{path}");
        let ev = evidence(repo, file, "deadbeef", 10, 42);
        let prov = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![ev],
            "t0.python",
            id.as_bytes(),
        )
        .unwrap();
        Node {
            id,
            label: "Endpoint".into(),
            props: serde_json::json!({
                "method": method,
                "path": path,
                "prov": prov,
            }),
        }
    }

    // AC-0313 (#739): canonical_endpoint and Markdown heading extraction match
    // `scripts/score-api-contract.mjs` over the shared fixture.
    #[test]
    fn canonical_parity_matches_scorer_fixture() {
        let fixture = include_str!("../tests/fixtures/api_contract_parity.md");
        let got: Vec<String> = documented_endpoints_markdown(fixture).into_iter().collect();
        assert_eq!(
            got,
            vec![
                "DELETE /",
                "GET /api/aliases/{}",
                "GET /api/edge/:/<>",
                "PATCH /api/profile/{}",
                "POST /api/aliases/{}/items/{}",
                "PUT /api/v2/settings",
            ]
        );
    }

    // AC-0313 (#739): Markdown and OpenAPI 3 JSON parse into Confirmed T0
    // DocumentedRoute nodes with exact spans, duplicate headings collapse into
    // one node citing every occurrence, and YAML / unknown extensions / zero
    // routes / non-3.x OpenAPI surface as explicit Gaps linked to their doc.
    #[test]
    fn markdown_and_openapi_json_parse_spans_and_collapse_duplicates() {
        let md = "# API\n#### GET /api/users/:id\nBody text\n#### GET /api/users/{user_id}\n";
        let first_line = "#### GET /api/users/:id";
        let second_line = "#### GET /api/users/{user_id}";
        let first_start = md.find(first_line).unwrap() as u64;
        let first_end = first_start + first_line.len() as u64;
        let second_start = md.find(second_line).unwrap() as u64;
        let second_end = second_start + second_line.len() as u64;

        let openapi = r#"{
  "openapi": "3.1.0",
  "paths": {
    "/api/orders/{order_id}": {
      "post": { "summary": "Create" },
      "delete": { "summary": "Delete" },
      "options": { "summary": "Ignored" }
    }
  }
}"#;
        let path_token = "\"/api/orders/{order_id}\"";
        let json_key_start = openapi.find(path_token).unwrap() as u64;
        let json_key_end = json_key_start + path_token.len() as u64;

        let docs = vec![
            DeclaredApiDoc {
                path: "docs/api.md".into(),
                prefix: Some("/api".into()),
                contents: md.into(),
            },
            DeclaredApiDoc {
                path: "docs/openapi.json".into(),
                prefix: Some("/api".into()),
                contents: openapi.into(),
            },
            DeclaredApiDoc {
                path: "docs/openapi.yaml".into(),
                prefix: None,
                contents: "openapi: 3.0.0\n".into(),
            },
            DeclaredApiDoc {
                path: "docs/notes.txt".into(),
                prefix: None,
                contents: "#### GET /api/ignored\n".into(),
            },
            DeclaredApiDoc {
                path: "docs/empty.md".into(),
                prefix: None,
                contents: "# Just prose\nNo endpoints here.\n".into(),
            },
            DeclaredApiDoc {
                path: "docs/swagger2.json".into(),
                prefix: None,
                contents: r#"{"openapi":"2.0","paths":{"/api/x":{"get":{}}}}"#.into(),
            },
        ];

        let facts = extract_contract_facts("acme/svc", "abc1234", &docs, &[], &[]);
        let routes: Vec<&Node> = facts
            .nodes
            .iter()
            .filter(|n| n.label == "DocumentedRoute")
            .collect();
        assert_eq!(routes.len(), 3);

        let md_route = routes
            .iter()
            .find(|n| n.props["doc_path"] == "docs/api.md")
            .unwrap();
        assert_eq!(
            md_route.id,
            "doc-route:acme/svc@docs/api.md#GET /api/users/{}"
        );
        assert_eq!(md_route.props["method"], "GET");
        assert_eq!(md_route.props["path"], "/api/users/:id");
        assert_eq!(md_route.props["canonical"], "GET /api/users/{}");
        let md_prov = typed_provenance(&md_route.props).unwrap();
        assert_eq!(md_prov.tier, Tier::Deterministic);
        assert_eq!(md_prov.confidence_tier, ConfidenceTier::Confirmed);
        assert_eq!(md_prov.evidence.len(), 2);
        assert_eq!(
            (md_prov.evidence[0].byte_start, md_prov.evidence[0].byte_end),
            (first_start, first_end)
        );
        assert_eq!(
            (md_prov.evidence[1].byte_start, md_prov.evidence[1].byte_end),
            (second_start, second_end)
        );
        assert!(facts.edges.iter().any(|e| {
            e.src == md_route.id
                && e.dst == "file:acme/svc@docs/api.md"
                && e.label == "DOCUMENTED_IN"
        }));

        let json_post = routes
            .iter()
            .find(|n| n.props["canonical"] == "POST /api/orders/{}")
            .unwrap();
        let json_prov = typed_provenance(&json_post.props).unwrap();
        assert_eq!(json_prov.evidence.len(), 1);
        assert_eq!(
            (
                json_prov.evidence[0].byte_start,
                json_prov.evidence[0].byte_end
            ),
            (json_key_start, json_key_end)
        );

        let gaps: Vec<&Node> = facts.nodes.iter().filter(|n| is_gap_node(n)).collect();
        assert_eq!(gaps.len(), 4);
        assert!(gaps.iter().any(|g| {
            g.props["doc_path"] == "docs/openapi.yaml"
                && g.props["reason"] == UNSUPPORTED_YAML_REASON
        }));
        assert!(gaps.iter().any(|g| {
            g.props["doc_path"] == "docs/notes.txt"
                && g.props["reason"]
                    .as_str()
                    .unwrap_or("")
                    .contains("unsupported API reference format")
        }));
        let empty_gap = gaps
            .iter()
            .find(|g| g.props["doc_path"] == "docs/empty.md")
            .unwrap();
        assert_eq!(empty_gap.props["reason"], EMPTY_DOC_REASON);
        assert!(facts.edges.iter().any(|e| {
            e.src == empty_gap.id
                && e.dst == "file:acme/svc@docs/empty.md"
                && e.label == "DOCUMENTED_IN"
        }));
        assert!(gaps.iter().any(|g| {
            g.props["doc_path"] == "docs/swagger2.json"
                && g.props["reason"]
                    .as_str()
                    .unwrap_or("")
                    .contains("expected \"3.x\"")
        }));
    }

    // AC-0550 (#1574): Endpoint nodes plus an id test give the same facts as
    // every recovered node: a recovered doc File suppresses the emitted one,
    // and non-Endpoint nodes are never matched as routes.
    #[test]
    fn contract_facts_among_endpoints_and_ids_match_all_candidates() {
        let docs = vec![
            DeclaredApiDoc {
                path: "docs/api.md".into(),
                prefix: Some("/api".into()),
                contents: "#### GET /api/orders\n#### POST /api/missing\n".into(),
            },
            DeclaredApiDoc {
                path: "docs/other.md".into(),
                prefix: None,
                contents: "#### GET /health\n".into(),
            },
        ];
        let gaps = vec![ContractValidationGap {
            doc_path: "docs/bad.yaml".into(),
            reason: "unparseable".into(),
        }];
        let mut not_a_route = endpoint_node("acme/svc", "GET", "/api/orders", "app.py");
        not_a_route.id = "sym:acme/svc@app.py#orders".into();
        not_a_route.label = "Symbol".into();
        let candidates = vec![
            Node {
                id: "file:acme/svc@docs/api.md".into(),
                label: "File".into(),
                props: serde_json::json!({}),
            },
            not_a_route,
            endpoint_node("acme/svc", "GET", "/api/orders", "app.py"),
            endpoint_node("acme/svc", "DELETE", "/api/orders", "app.py"),
        ];
        let all = extract_contract_facts("acme/svc", "abc1234", &docs, &gaps, &candidates);
        let endpoints: Vec<Node> = candidates
            .iter()
            .filter(|node| node.label == "Endpoint")
            .cloned()
            .collect();
        let ids: BTreeSet<&str> = candidates.iter().map(|node| node.id.as_str()).collect();
        let among =
            extract_contract_facts_among("acme/svc", "abc1234", &docs, &gaps, &endpoints, |id| {
                ids.contains(id)
            });
        assert_eq!(among, all);
        assert!(!all.nodes.is_empty());
        assert!(
            !all.nodes
                .iter()
                .any(|node| node.id == "file:acme/svc@docs/api.md"),
            "a recovered doc File is not emitted again"
        );
    }

    // AC-0313 (#739): served-side findings never claim more certainty than the
    // recovered endpoints they cite; an InferredStrong endpoint (for example a
    // route on an unmounted router) yields InferredStrong drift and edges.
    #[test]
    fn contract_drift_inherits_weakest_endpoint_confidence() {
        let docs = vec![DeclaredApiDoc {
            path: "docs/api.md".into(),
            prefix: Some("/api".into()),
            contents: "#### POST /api/disjoint\n".into(),
        }];
        let inferred = |method: &str, path: &str| {
            let mut node = endpoint_node("acme/svc", method, path, "app.py");
            let mut prov = typed_provenance(&node.props).unwrap();
            prov.confidence_tier = ConfidenceTier::InferredStrong;
            node.props["prov"] = serde_json::to_value(prov).unwrap();
            node
        };
        let candidates = vec![
            inferred("GET", "/api/unmounted"),
            inferred("PUT", "/api/disjoint"),
            endpoint_node("acme/svc", "GET", "/api/confirmed", "app.py"),
        ];
        let facts = extract_contract_facts("acme/svc", "abc1234", &docs, &[], &candidates);
        let tier_of = |props: &serde_json::Value| typed_provenance(props).unwrap().confidence_tier;
        let drift = |key: &str| {
            facts
                .nodes
                .iter()
                .find(|n| is_drift_node(n) && n.props["key"] == key)
                .unwrap()
        };
        for (key, expected) in [
            ("GET /api/unmounted", ConfidenceTier::InferredStrong),
            ("/api/disjoint", ConfidenceTier::InferredStrong),
            ("GET /api/confirmed", ConfidenceTier::Confirmed),
        ] {
            let node = drift(key);
            assert_eq!(tier_of(&node.props), expected, "{key}");
            for edge in facts
                .edges
                .iter()
                .filter(|e| e.src == node.id && e.label == "SERVED_AS")
            {
                assert_eq!(tier_of(&edge.props), expected, "{key} SERVED_AS");
            }
        }
        assert_eq!(
            weakest_confidence([ConfidenceTier::Confirmed, ConfidenceTier::InferredWeak]),
            ConfidenceTier::InferredWeak
        );
    }

    // AC-0347 (#968): GraphQL operations are not HTTP routes, so HTTP API
    // docs neither match them nor report them served-undocumented.
    #[test]
    fn contract_drift_ignores_graphql_operations() {
        let docs = vec![DeclaredApiDoc {
            path: "docs/api.md".into(),
            prefix: None,
            contents: "#### GET /api/items\n".into(),
        }];
        let mut graphql = endpoint_node("acme/svc", "QUERY", "admin.customers", "r.ts");
        graphql.props["protocol"] = "graphql".into();
        let candidates = vec![
            graphql,
            endpoint_node("acme/svc", "GET", "/api/items", "app.py"),
        ];
        let facts = extract_contract_facts("acme/svc", "abc1234", &docs, &[], &candidates);
        assert!(
            !facts
                .nodes
                .iter()
                .any(|n| n.props.to_string().contains("admin.customers"))
        );
        assert!(
            !facts
                .nodes
                .iter()
                .any(|n| n.props.to_string().contains("unusable"))
        );
    }

    // AC-0313 (#739): drift comparison covers documented-not-served
    // (InferredStrong), served-undocumented (Confirmed), method-mismatch
    // replacing disjoint pairs (Confirmed), partial method overlap, prefix
    // scoping vs default first-segment scoping, and unusable endpoint counts.
    #[test]
    fn contract_drift_recovers_not_served_undocumented_and_method_mismatch() {
        let md = "\
#### GET /api/matched/:id
#### DELETE /api/missing/:id
#### POST /api/disjoint/:id
#### GET /api/overlap
#### POST /api/overlap
";
        let docs = vec![DeclaredApiDoc {
            path: "docs/api.md".into(),
            prefix: None, // default first-segment scoping -> "api"
            contents: md.into(),
        }];
        let mut unusable = endpoint_node("acme/svc", "GET", "/api/broken", "app.py");
        unusable.props.as_object_mut().unwrap().remove("path");

        let candidates = vec![
            endpoint_node("acme/svc", "GET", "/api/matched/<int:id>", "app.py"),
            // Disjoint method on /api/disjoint/{}: doc has POST, code serves PUT.
            endpoint_node("acme/svc", "PUT", "/api/disjoint/{id}", "app.py"),
            // Partial overlap on /api/overlap: doc has GET+POST, code serves GET+DELETE.
            endpoint_node("acme/svc", "GET", "/api/overlap", "app.py"),
            endpoint_node("acme/svc", "DELETE", "/api/overlap", "app.py"),
            // In-scope undocumented (first segment "api").
            endpoint_node("acme/svc", "GET", "/api/internal", "app.py"),
            // Out-of-scope (first segment "admin" != "api").
            endpoint_node("acme/svc", "GET", "/admin/metrics", "app.py"),
            unusable,
        ];

        let facts = extract_contract_facts("acme/svc", "abc1234", &docs, &[], &candidates);
        let drifts: Vec<&Node> = facts.nodes.iter().filter(|n| is_drift_node(n)).collect();
        let mut summary: Vec<(&str, &str, ConfidenceTier)> = drifts
            .iter()
            .map(|n| {
                let p = typed_provenance(&n.props).unwrap();
                (
                    n.props["drift_kind"].as_str().unwrap(),
                    n.props["key"].as_str().unwrap(),
                    p.confidence_tier,
                )
            })
            .collect();
        summary.sort_by_key(|(k, e, _)| (*k, *e));

        assert_eq!(
            summary,
            vec![
                (
                    "documented-not-served",
                    "DELETE /api/missing/{}",
                    ConfidenceTier::InferredStrong,
                ),
                (
                    "documented-not-served",
                    "POST /api/overlap",
                    ConfidenceTier::InferredStrong,
                ),
                (
                    "method-mismatch",
                    "/api/disjoint/{}",
                    ConfidenceTier::Confirmed,
                ),
                (
                    "served-undocumented",
                    "DELETE /api/overlap",
                    ConfidenceTier::Confirmed,
                ),
                (
                    "served-undocumented",
                    "GET /api/internal",
                    ConfidenceTier::Confirmed,
                ),
            ]
        );

        let not_served = drifts
            .iter()
            .find(|n| n.props["key"] == "DELETE /api/missing/{}")
            .unwrap();
        assert_eq!(not_served.props["reason"], DOCUMENTED_NOT_SERVED_REASON);
        assert_eq!(
            not_served.props["prov"]["reason"],
            DOCUMENTED_NOT_SERVED_REASON
        );
        assert!(facts.edges.iter().any(|e| {
            e.src == not_served.id
                && e.dst == "doc-route:acme/svc@docs/api.md#DELETE /api/missing/{}"
                && e.label == "DOCUMENTED_AS"
        }));

        let mismatch = drifts
            .iter()
            .find(|n| n.props["drift_kind"] == "method-mismatch")
            .unwrap();
        assert!(facts.edges.iter().any(|e| {
            e.src == mismatch.id
                && e.dst == "doc-route:acme/svc@docs/api.md#POST /api/disjoint/{}"
                && e.label == "DOCUMENTED_AS"
        }));
        assert!(facts.edges.iter().any(|e| {
            e.src == mismatch.id
                && e.dst == "ep:acme/svc@PUT:/api/disjoint/{id}"
                && e.label == "SERVED_AS"
        }));

        // Explicit prefix scoping restricts served-undocumented to routes under prefix.
        let scoped_docs = vec![DeclaredApiDoc {
            path: "docs/api.md".into(),
            prefix: Some("/api/v2".into()),
            contents: "#### GET /api/v2/items\n".into(),
        }];
        let scoped_candidates = vec![
            endpoint_node("acme/svc", "GET", "/api/v2/items", "app.py"),
            endpoint_node("acme/svc", "GET", "/api/v1/legacy", "app.py"),
            endpoint_node("acme/svc", "POST", "/api/v2/extra", "app.py"),
        ];
        let scoped_facts =
            extract_contract_facts("acme/svc", "abc1234", &scoped_docs, &[], &scoped_candidates);
        let scoped_drifts: Vec<&Node> = scoped_facts
            .nodes
            .iter()
            .filter(|n| is_drift_node(n))
            .collect();
        assert_eq!(scoped_drifts.len(), 1);
        assert_eq!(scoped_drifts[0].props["key"], "POST /api/v2/extra");

        // Unusable endpoint count is reported in a Gap node's text.
        assert!(facts.nodes.iter().any(|n| {
            is_gap_node(n)
                && n.props["reason"]
                    .as_str()
                    .unwrap_or("")
                    .contains("1 recovered Endpoint node(s) without method and path props")
        }));
    }

    // AC-0313 (#739): drift_register.md keeps ADR-only output byte-identical
    // when no docs are declared, and renders the sorted `## API contract drift`
    // section with Drift node/edge assertions and doc Gaps when docs exist.
    #[test]
    fn drift_register_renders_contract_section_and_preserves_adr_only_bytes() {
        let empty_bundle = compile_spec(&[], &[], &[], ExportMode::VerifiedOnly, &BTreeSet::new());
        let empty_drift = empty_bundle
            .artifacts
            .iter()
            .find(|a| a.id == "drift-register")
            .unwrap();
        assert_eq!(
            empty_drift.content,
            "# Drift register\n\n| Finding | ADR | Offending edge | Flow triggers | Confidence |\n|---|---|---|---|---|\n| — | No ADR/code conflicts recovered |\n\n## Assertions and inline provenance\n\nNo graph-backed assertions were recovered for this artifact.\n"
        );

        let docs = vec![
            DeclaredApiDoc {
                path: "docs/api.md".into(),
                prefix: Some("/api".into()),
                contents: "#### GET /api/users/:id\n#### POST /api/orders\n".into(),
            },
            DeclaredApiDoc {
                path: "docs/empty.md".into(),
                prefix: Some("/api".into()),
                contents: "# Empty\n".into(),
            },
        ];
        let candidates = vec![
            endpoint_node("acme/svc", "PUT", "/api/orders", "app.py"),
            endpoint_node("acme/svc", "GET", "/api/undoc", "app.py"),
        ];
        let mut nodes = candidates.clone();
        let facts = extract_contract_facts(
            "acme/svc",
            "abc1234",
            &docs,
            &[ContractValidationGap {
                doc_path: "docs/bad.md".into(),
                reason: "declared API doc path \"../bad.md\" must not contain '..'".into(),
            }],
            &candidates,
        );
        nodes.extend(facts.nodes);

        let bundle = compile_spec(
            &nodes,
            &facts.edges,
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );
        assert_eq!(bundle.drift_count, 3);
        let drift_art = bundle
            .artifacts
            .iter()
            .find(|a| a.id == "drift-register")
            .unwrap();
        assert!(drift_art.content.contains("\n## API contract drift\n\n| Finding | Endpoint | Documented | Served | Confidence |\n|---|---|---|---|---|\n"));

        let dns_pos = drift_art
            .content
            .find("| documented-not-served | `GET /api/users/{}` |")
            .unwrap();
        let mm_pos = drift_art
            .content
            .find("| method-mismatch | `/api/orders` |")
            .unwrap();
        let su_pos = drift_art
            .content
            .find("| served-undocumented | `GET /api/undoc` |")
            .unwrap();
        assert!(dns_pos < mm_pos && mm_pos < su_pos);
        assert!(drift_art.content.contains(
            "| gap | `docs/empty.md` | declared API reference has no parsable routes | — | Gap |"
        ));
        assert!(drift_art.content.contains("| gap | `docs/bad.md` | declared API doc path \"../bad.md\" must not contain '..' | — | Gap |"));

        // Every contract Drift node and DOCUMENTED_AS / SERVED_AS edge is in assertions.
        for edge in facts.edges.iter().filter(|e| is_drift_edge(e)) {
            assert!(
                drift_art
                    .assertions
                    .iter()
                    .any(|a| a.edge_label.as_deref() == Some(edge.label.as_str())),
                "missing assertion for edge {:?}",
                edge
            );
        }
    }
}
