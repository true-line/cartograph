use super::*;
use crate::HopKind;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};
fn invalid(s: &'static str) -> FixtureError {
    FixtureError::Invalid(s)
}
fn ensure(ok: bool, why: &'static str) -> Result<()> {
    if ok { Ok(()) } else { Err(invalid(why)) }
}
fn text(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.chars().count())
}
fn hash(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn id(s: &str) -> bool {
    s.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}
pub(super) fn path(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('/')
        && !s.chars().any(|c| c.is_control() || c == '\\' || c == ':')
        && s.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}
/// Evaluate just the keywords used by the frozen schema. No remote references,
/// caller-selected schemas or regex programs enter this boundary.
pub(super) fn shape(v: &Value, name: &str, purpose: ExpectedPurpose) -> Result<()> {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    let schema = SCHEMA.get_or_init(|| {
        serde_json::from_str(include_str!("schema.json")).expect("checked frozen schema")
    });
    let def = schema["$defs"]
        .get(name)
        .ok_or(invalid("unknown fixture definition"))?;
    if matches!(
        name,
        "InputsManifest" | "LeakageManifest" | "LabelsManifest"
    ) {
        // Only the root purpose constant differs for independently reviewed
        // datasets. Never change raw bytes, normalized hashes or frozen schema.
        let mut definition = def.clone();
        definition["properties"]["purpose"]["const"] = Value::String(purpose.value().into());
        if matches!(purpose, ExpectedPurpose::IndependentEvaluation) && name == "LeakageManifest" {
            definition["properties"]["partition_policy"]["properties"]["version"]["const"] =
                Value::String("catalogue-leakage@1".into());
        }
        check_shape(v, &definition, schema)
    } else {
        check_shape(v, def, schema)
    }
}
fn check_shape(v: &Value, s: &Value, root: &Value) -> Result<()> {
    if let Some(r) = s["$ref"].as_str() {
        return check_shape(
            v,
            &root["$defs"][r
                .strip_prefix("#/$defs/")
                .ok_or(invalid("schema reference"))?],
            root,
        );
    }
    if let Some(a) = s["oneOf"].as_array() {
        return ensure(
            a.iter().filter(|s| check_shape(v, s, root).is_ok()).count() == 1,
            "fixture union shape",
        );
    }
    if let Some(a) = s["anyOf"].as_array() {
        return ensure(
            a.iter().any(|s| check_shape(v, s, root).is_ok()),
            "fixture nullable shape",
        );
    }
    if let Some(c) = s.get("const") {
        ensure(v == c, "fixture constant")?;
    }
    if let Some(e) = s["enum"].as_array() {
        ensure(e.contains(v), "fixture enum")?;
    }
    match s["type"].as_str() {
        Some("object") => {
            let m = v.as_object().ok_or(invalid("fixture object"))?;
            let p = s["properties"]
                .as_object()
                .ok_or(invalid("schema properties"))?;
            ensure(
                m.len() == p.len() && m.keys().all(|k| p.contains_key(k)),
                "closed required object",
            )?;
            for (k, p) in p {
                check_shape(m.get(k).ok_or(invalid("required field"))?, p, root)?;
            }
        }
        Some("array") => {
            let a = v.as_array().ok_or(invalid("fixture array"))?;
            ensure(
                (s["minItems"].as_u64().unwrap_or(0) as usize
                    ..=s["maxItems"].as_u64().unwrap_or(1024) as usize)
                    .contains(&a.len()),
                "fixture array bound",
            )?;
            for item in a {
                check_shape(item, &s["items"], root)?;
            }
            if s["uniqueItems"] == true {
                let mut set = BTreeSet::new();
                for item in a {
                    ensure(set.insert(canonical_json(item)?), "duplicate array item")?;
                }
            }
        }
        Some("string") => {
            let t = v.as_str().ok_or(invalid("fixture string"))?;
            ensure(
                text(
                    t,
                    s["minLength"].as_u64().unwrap_or(0) as usize,
                    s["maxLength"].as_u64().unwrap_or(8192) as usize,
                ),
                "fixture string bound",
            )?;
            if let Some(p) = s["pattern"].as_str() {
                let ok = if p.contains("{40}") {
                    hash(t, 40)
                } else if p.contains("{64}") {
                    hash(t, 64)
                } else if p.starts_with("^[A-Za-z0-9]") {
                    id(t)
                } else {
                    path(t)
                };
                ensure(ok, "fixture string pattern")?;
            }
        }
        Some("integer") => {
            let n = v
                .as_u64()
                .ok_or(invalid("unsigned bounded fixture integer"))?;
            ensure(
                n >= s["minimum"].as_u64().unwrap_or(0)
                    && n <= s["maximum"].as_u64().unwrap_or(u64::MAX),
                "fixture integer bound",
            )?;
        }
        Some("null") => ensure(v.is_null(), "fixture null")?,
        None => {}
        _ => return Err(invalid("unsupported frozen schema type")),
    }
    Ok(())
}
fn sorted<'a>(iter: impl Iterator<Item = &'a str>) -> bool {
    let mut last = None;
    for x in iter {
        if last.is_some_and(|p| p >= x) {
            return false;
        }
        last = Some(x);
    }
    true
}
fn evidence_key(e: &EvidenceRef) -> (&str, &str, u64, u64, &str) {
    (&e.repo, &e.path, e.byte_start, e.byte_end, &e.commit_sha)
}
fn evidence_sorted(e: &[EvidenceRef]) -> bool {
    e.windows(2)
        .all(|w| evidence_key(&w[0]) < evidence_key(&w[1]))
}
fn ids_exist(ids: &[String], sources: &BTreeMap<&str, &RetainedSource>) -> Result<()> {
    ensure(
        ids.iter().all(|id| sources.contains_key(id.as_str())),
        "unknown retained source",
    )
}
fn ev_matches(
    e: &[EvidenceRef],
    ids: &[String],
    sources: &BTreeMap<&str, &RetainedSource>,
) -> Result<()> {
    ensure(evidence_sorted(e), "unsorted evidence")?;
    ids_exist(ids, sources)?;
    for r in e {
        ensure(
            r.byte_end > r.byte_start
                && ids
                    .iter()
                    .any(|id| sources.get(id.as_str()).is_some_and(|s| &s.evidence == r)),
            "evidence without exact retained bytes",
        )?;
    }
    Ok(())
}
pub(super) fn validate(
    i: &InputsManifest,
    l: &LeakageManifest,
    b: &LabelsManifest,
    d: &FixtureDigests,
) -> Result<()> {
    ensure(
        i.dataset_id == l.dataset_id && i.dataset_id == b.dataset_id,
        "dataset mismatch",
    )?;
    ensure(
        l.inputs_hash == d.inputs && b.inputs_hash == d.inputs && b.leakage_hash == d.leakage,
        "manifest digest mismatch",
    )?;
    ensure(
        sorted(i.corpora.iter().map(|x| x.corpus_id.as_str()))
            && sorted(i.sources.iter().map(|x| x.source_id.as_str()))
            && sorted(i.cases.iter().map(|x| x.case_id.as_str()))
            && sorted(l.groups.iter().map(|x| x.group_id.as_str()))
            && sorted(b.labels.iter().map(|x| x.case_id.as_str())),
        "unsorted or duplicate manifest IDs",
    )?;
    let corpora: BTreeMap<_, _> = i
        .corpora
        .iter()
        .map(|x| (x.corpus_id.as_str(), x))
        .collect();
    let sources: BTreeMap<_, _> = i
        .sources
        .iter()
        .map(|x| (x.source_id.as_str(), x))
        .collect();
    let cases: BTreeMap<_, _> = i.cases.iter().map(|x| (x.case_id.as_str(), x)).collect();
    for s in &i.sources {
        let c = corpora
            .get(s.corpus_id.as_str())
            .ok_or(invalid("source corpus"))?;
        ensure(
            s.evidence.repo == c.repo_id
                && s.evidence.commit_sha == c.revision
                && s.evidence.byte_end.checked_sub(s.evidence.byte_start) == Some(s.byte_len),
            "source span corpus binding",
        )?;
    }
    for c in &i.cases {
        ensure(corpora.contains_key(c.corpus_id.as_str()), "case corpus")?;
        ensure(
            sorted(c.source_ids.iter().map(String::as_str)),
            "unsorted source IDs",
        )?;
        ev_matches(&c.hop.evidence, &c.source_ids, &sources)?;
        ensure(
            c.source_ids
                .iter()
                .all(|id| sources[id.as_str()].corpus_id == c.corpus_id),
            "hop corpus binding",
        )?;
        ensure(
            match c.family.kind() {
                HopKind::Call => c.hop.edge_label == EdgeLabel::CALLS,
                HopKind::Channel => matches!(
                    c.hop.edge_label,
                    EdgeLabel::PUBLISHES | EdgeLabel::SUBSCRIBES
                ),
            },
            "family edge binding",
        )?;
        ensure(
            c.hop.query_hash == sha256(c.hop.query.as_bytes()),
            "query hash",
        )?;
        // AC-0285: an interface-dispatch query (`IFace.method`) is classified
        // out of ANN candidacy, so a dataset case carrying one could never be
        // planned, embedded or scored; reject it at validation.
        ensure(
            !crate::is_interface_dispatch(&c.hop.query),
            "interface dispatch hop query in dataset",
        )?;
        ensure(
            sorted(c.candidates.iter().map(|x| x.node_id.as_str())),
            "duplicate or unsorted candidate IDs",
        )?;
        for candidate in &c.candidates {
            ensure(
                candidate.family == c.family
                    && candidate.text_hash == sha256(candidate.text.as_bytes()),
                "candidate family or text hash",
            )?;
            ensure(
                sorted(candidate.source_ids.iter().map(String::as_str)),
                "unsorted candidate source IDs",
            )?;
            ev_matches(&candidate.evidence, &candidate.source_ids, &sources)?;
        }
        ensure(
            c.candidate_set_hash == sha256(&canonical_json(&c.candidates)?),
            "candidate set hash",
        )?;
    }
    let catalogue = i.purpose == "independent_evaluation";
    ensure(
        l.partition_policy.version
            == if catalogue {
                "catalogue-leakage@1"
            } else {
                "sha256-group-bucket@1"
            },
        "purpose-specific leakage rule mismatch",
    )?;
    let labels_by_case: BTreeMap<_, _> = b
        .labels
        .iter()
        .map(|label| (label.case_id.as_str(), label))
        .collect();
    ensure(
        labels_by_case.len() == cases.len()
            && labels_by_case.keys().all(|id| cases.contains_key(id)),
        "incomplete label coverage",
    )?;
    let mut query_sources: BTreeMap<&str, &str> = BTreeMap::new();
    let mut raw_queries: BTreeMap<&str, &str> = BTreeMap::new();
    let mut membership = BTreeMap::new();
    let mut source_splits: BTreeMap<&str, &str> = BTreeMap::new();
    let mut query_splits: BTreeMap<(&FamilyKey, &str), &str> = BTreeMap::new();
    let mut target_splits: BTreeMap<(&FamilyKey, &str), &str> = BTreeMap::new();
    let mut span_splits: BTreeMap<(&str, &str, u64, u64), &str> = BTreeMap::new();
    let mut body_splits: BTreeMap<&str, &str> = BTreeMap::new();
    for g in &l.groups {
        ensure(
            sorted(g.case_ids.iter().map(String::as_str))
                && sorted(g.related_source_ids.iter().map(String::as_str)),
            "unsorted leakage IDs",
        )?;
        ids_exist(&g.related_source_ids, &sources)?;
        let bases: BTreeSet<_> = g.grouping_basis.iter().collect();
        ensure(
            bases.len() == g.grouping_basis.len()
                && (!bases.contains(&GroupingBasis::Independent) || bases.len() == 1),
            "grouping basis",
        )?;
        ensure(
            partition(&l.partition_policy, &g.group_id) == g.split,
            "partition mismatch",
        )?;
        let mut supervised_group_sources: BTreeSet<&str> = BTreeSet::new();
        if !catalogue {
            for id in &g.related_source_ids {
                same_group(&mut source_splits, id.as_str(), g.group_id.as_str())?;
                let s = sources[id.as_str()];
                same_group(
                    &mut span_splits,
                    (
                        &s.evidence.repo,
                        &s.evidence.path,
                        s.evidence.byte_start,
                        s.evidence.byte_end,
                    ),
                    g.group_id.as_str(),
                )?;
                same_group(
                    &mut body_splits,
                    s.content_hash.as_str(),
                    g.group_id.as_str(),
                )?;
            }
        }
        for case_id in &g.case_ids {
            let c = cases
                .get(case_id.as_str())
                .ok_or(invalid("unknown grouped case"))?;
            ensure(
                membership
                    .insert(case_id.as_str(), g.group_id.as_str())
                    .is_none(),
                "case in multiple groups",
            )?;
            let labelled_ids: BTreeSet<&str> = if catalogue {
                let label = labels_by_case
                    .get(case_id.as_str())
                    .ok_or(invalid("missing case label"))?;
                let mut ids: BTreeSet<&str> = label
                    .hard_negative_target_ids
                    .iter()
                    .map(String::as_str)
                    .collect();
                if let Truth::AcceptableTargets { target_ids } = &label.truth {
                    ids.extend(target_ids.iter().map(String::as_str));
                }
                ids
            } else {
                c.candidates.iter().map(|c| c.node_id.as_str()).collect()
            };
            if catalogue {
                let label = labels_by_case[case_id.as_str()];
                let mut supervised_sources: BTreeSet<&str> = c
                    .source_ids
                    .iter()
                    .chain(&label.origin.source_ids)
                    .map(String::as_str)
                    .collect();
                for target in &labelled_ids {
                    let candidate = c
                        .candidates
                        .iter()
                        .find(|c| c.node_id == *target)
                        .ok_or(invalid("labelled target outside universe"))?;
                    supervised_sources.extend(candidate.source_ids.iter().map(String::as_str));
                }
                for id in &supervised_sources {
                    let source = sources
                        .get(id)
                        .ok_or(invalid("missing supervised source"))?;
                    same_group(&mut source_splits, *id, g.group_id.as_str())?;
                    same_group(
                        &mut span_splits,
                        (
                            &source.evidence.repo,
                            &source.evidence.path,
                            source.evidence.byte_start,
                            source.evidence.byte_end,
                        ),
                        g.group_id.as_str(),
                    )?;
                    same_group(
                        &mut body_splits,
                        source.content_hash.as_str(),
                        g.group_id.as_str(),
                    )?;
                }
                supervised_group_sources.extend(supervised_sources);
                same_group(
                    &mut query_sources,
                    c.hop.source_id.as_str(),
                    g.group_id.as_str(),
                )?;
                same_group(
                    &mut raw_queries,
                    c.hop.query_hash.as_str(),
                    g.group_id.as_str(),
                )?;
            } else {
                let all_ids = c
                    .source_ids
                    .iter()
                    .chain(c.candidates.iter().flat_map(|c| c.source_ids.iter()));
                ensure(
                    all_ids
                        .clone()
                        .all(|id| g.related_source_ids.binary_search(id).is_ok()),
                    "missing related source",
                )?;
            }
            same_group(
                &mut query_splits,
                (&c.family, c.hop.query_hash.as_str()),
                g.group_id.as_str(),
            )?;
            for candidate in c
                .candidates
                .iter()
                .filter(|candidate| labelled_ids.contains(candidate.node_id.as_str()))
            {
                same_group(
                    &mut target_splits,
                    (&candidate.family, candidate.node_id.as_str()),
                    g.group_id.as_str(),
                )?;
            }
        }
        if catalogue {
            ensure(
                supervised_group_sources
                    == g.related_source_ids.iter().map(String::as_str).collect(),
                "catalogue rule related sources must be exact supervised union",
            )?;
        }
    }
    ensure(
        membership.len() == cases.len() && b.labels.len() == cases.len(),
        "incomplete case membership or labels",
    )?;
    for label in &b.labels {
        let c = cases
            .get(label.case_id.as_str())
            .ok_or(invalid("unknown labeled case"))?;
        ensure(
            sorted(label.hard_negative_target_ids.iter().map(String::as_str))
                && sorted(label.origin.source_ids.iter().map(String::as_str)),
            "unsorted label IDs",
        )?;
        ids_exist(&label.origin.source_ids, &sources)?;
        let targets: BTreeSet<_> = c.candidates.iter().map(|x| x.node_id.as_str()).collect();
        ensure(
            label
                .hard_negative_target_ids
                .iter()
                .all(|x| targets.contains(x.as_str())),
            "hard negative outside universe",
        )?;
        match &label.truth {
            Truth::AcceptableTargets { target_ids } => {
                ensure(
                    sorted(target_ids.iter().map(String::as_str))
                        && target_ids.iter().all(|id| {
                            targets.contains(id.as_str())
                                && !label.hard_negative_target_ids.contains(id)
                        }),
                    "acceptable target binding",
                )?;
                ensure(
                    label
                        .origin
                        .exclusivity_basis
                        .as_ref()
                        .is_some_and(|x| !x.trim().is_empty()),
                    "missing exclusivity basis",
                )?;
            }
            Truth::NoMatch => ensure(
                label
                    .origin
                    .exclusivity_basis
                    .as_ref()
                    .is_some_and(|x| !x.trim().is_empty()),
                "missing no-match exclusivity",
            )?,
            Truth::Unknown { .. } => ensure(
                label.origin.exclusivity_basis.is_none()
                    && label.hard_negative_target_ids.is_empty(),
                "unknown mislabeled as negative",
            )?,
        }
    }
    Ok(())
}
fn same_group<'a, K: Ord>(m: &mut BTreeMap<K, &'a str>, key: K, group: &'a str) -> Result<()> {
    ensure(
        m.insert(key, group).is_none_or(|old| old == group),
        "related cases in separate leakage groups",
    )
}
pub(super) fn partition(p: &PartitionPolicy, id: &str) -> Split {
    let mut h = Sha256::new();
    h.update(b"cartograph-t2-split@1\0");
    h.update(p.salt.as_bytes());
    h.update(b"\0");
    h.update(id.as_bytes());
    let d = h.finalize();
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 10000;
    if u64::from(n) < p.calibration_buckets {
        Split::Calibration
    } else {
        Split::Evaluation
    }
}
