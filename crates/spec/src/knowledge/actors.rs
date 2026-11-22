//! Endpoint-scoped caller sets under an explicit weak export-name convention.
use super::*;
use serde::{Deserialize, Serialize};

/// Rule projecting confirmed annotation syntax to weak caller-set candidates.
pub const ACTOR_ALLOW_RULE: &str = "knowledge.actor.allow-callers";
/// Bump if eligibility, payload or the assumption statement changes.
pub const ACTOR_ALLOW_RULE_VERSION: &str = "1";
/// Convention only: this does not attest effective runtime authorization.
pub const ACTOR_ALLOW_ASSUMPTION: &str = "Allow(...) admits callers holding any listed permission";

/// Individually admitted candidates; aggregate publication remains separate.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ActorReport {
    /// Weak node-only candidates citing their exact base Endpoint.
    pub facts: Vec<OverlayFact>,
    /// Candidates omitted under the existing per-fact JSON byte cap.
    pub oversized_endpoints: usize,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ImportBinding {
    exported: String,
    local: String,
    module: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PermissionSite {
    basis: String,
    arguments: Vec<String>,
    decorator: ImportBinding,
    permission_import: ImportBinding,
    prov: Provenance,
}
// Mirror tree-sitter-typescript 0.23.2's identifier token (also aliased to
// property_identifier): a negated delimiter class plus source-spelled escapes.
// Keep the existing control rejection; do not impose a narrower Unicode alphabet.
fn syntax_identifier(argument: &str, reject_public: bool) -> bool {
    let mut chars = argument.chars().peekable();
    let mut first = true;
    let mut public_prefix = Some(0);
    while let Some(c) = chars.next() {
        let mut decoded = c as u32;
        if c == '\\' {
            decoded = 0;
            if chars.next() != Some('u') {
                return false;
            }
            if chars.peek() == Some(&'{') {
                chars.next();
                let mut saw_digit = false;
                while chars.peek().is_some_and(char::is_ascii_hexdigit) {
                    let digit = chars.next().unwrap().to_digit(16).unwrap();
                    decoded = (decoded * 16 + digit).min(128);
                    saw_digit = true;
                }
                if !saw_digit || chars.next() != Some('}') {
                    return false;
                }
            } else {
                for _ in 0..4 {
                    let Some(c) = chars.next().filter(char::is_ascii_hexdigit) else {
                        return false;
                    };
                    decoded = (decoded * 16 + c.to_digit(16).unwrap()).min(128);
                }
            }
        } else if c.is_control()
            || c.is_whitespace()
            || (first && c.is_ascii_digit())
            || ":;`\"'@#.,|^&<=>+-*/%?!~()[]{}".contains(c)
            || matches!(
                c,
                '\u{feff}' | '\u{2060}' | '\u{200b}' | '\u{2028}' | '\u{2029}'
            )
        {
            return false;
        }
        // Decode only for the existing Public exclusion; preserve source spelling.
        // Capping escape values at 128 avoids overflow without scalar validation.
        public_prefix = public_prefix.and_then(|index| {
            (b"Public".get(index).map(|c| u32::from(*c)) == Some(decoded)).then_some(index + 1)
        });
        first = false;
    }
    !first && !(reject_public && public_prefix == Some(6))
}
fn site(node: &Node) -> Option<PermissionSite> {
    if node.label != "Endpoint"
        || node.id.is_empty()
        || node.props["protocol"] != "graphql"
        || node.props["framework"] != "nestjs-graphql"
    {
        return None;
    }
    let endpoint = support(&node.props)?;
    let raw = node.props.get("permission_site")?;
    let site = PermissionSite::deserialize(raw).ok()?;
    if site.basis != "import-bound-syntax"
        || site.arguments.is_empty()
        || site.arguments.iter().any(|s| !syntax_identifier(s, true))
        || node.props["permissions"] != serde_json::json!(site.arguments)
        || site.decorator.exported != "Allow"
        || site.permission_import.exported != "Permission"
        || [&site.decorator, &site.permission_import].iter().any(|b| {
            !syntax_identifier(&b.local, false)
                || b.module.trim().is_empty()
                || b.module.chars().any(char::is_control)
        })
        || site.prov.tier != Tier::Deterministic
        || site.prov.confidence_tier != ConfidenceTier::Confirmed
        || site.prov.basis.is_some()
        || !matches!(site.prov.method, ProducingMethod::Unrecorded)
        || serde_json::to_value(&site.prov).ok()? != raw["prov"]
        || !matches!(site.prov.evidence.len(), 4 | 5)
        || site
            .prov
            .evidence
            .windows(2)
            .any(|w| w[0].byte_end > w[1].byte_start)
    {
        return None;
    }
    let checked = support(raw)?;
    if !same_owner(&endpoint, &endpoint.evidence[0]) || !same_owner(&checked, &endpoint.evidence[0])
    {
        return None;
    }
    let mut payload = raw.as_object()?.clone();
    payload.remove("prov");
    let content = format!(
        "GraphQL permission syntax {}",
        serde_json::Value::Object(payload)
    );
    (checked.content_hash == core_prov::content_hash(content.as_bytes())).then_some(site)
}
fn actor(node: &Node, site: &PermissionSite, assumption: &str) -> OverlayFact {
    let mut props = serde_json::json!({
        "name":format!("Caller set for endpoint {}", node.id), "endpoint_id":node.id,
        "any_of":site.arguments, "permission_site":site,
        "assumption":{"name":"allow-any-listed-permission", "statement":assumption,
            "rule":ACTOR_ALLOW_RULE, "version":ACTOR_ALLOW_RULE_VERSION},
    });
    let content = serde_json::to_vec(&(ACTOR_ALLOW_RULE, ACTOR_ALLOW_RULE_VERSION, &props))
        .expect("actor content serializes");
    props["prov"] = serde_json::json!(
        Provenance::new(
            Tier::Semantic,
            ConfidenceTier::InferredWeak,
            site.prov.evidence.clone(),
            "t2.knowledge.actor.allow-callers",
            &content
        )
        .expect("semantic weak is within ceiling")
        .with_method(ProducingMethod::Rule {
            rule: ACTOR_ALLOW_RULE.into(),
            version: ACTOR_ALLOW_RULE_VERSION.into(),
        })
    );
    OverlayFact {
        item: OverlayItem::Node(Node {
            id: format!("actor:allow-callers:{}", core_prov::content_hash(&content)),
            label: "Actor".into(),
            props,
        }),
        cites: BTreeSet::from([BaseRef::Node(node.id.clone())]),
    }
}
/// Apply a named weak convention only to validated AC-0517 syntax. This checks
/// consistency, not source authenticity, runtime policy or current overlay currency.
/// The caller admits input/aggregate output; rendering the assumption is separate.
pub fn derive_permission_actors(base: &RevisionedSnapshot) -> ActorReport {
    // Every derivation under test reads a compacted base, as the knowledge
    // phase reads it (#2215).
    #[cfg(test)]
    let compacted = crate::workbench::compacted_snapshot_for_tests(base);
    #[cfg(test)]
    let base = &compacted;
    let mut nodes = BTreeMap::new();
    for n in &base.graph.0 {
        nodes
            .entry(&n.id)
            .and_modify(|v| *v = None)
            .or_insert(Some(n));
    }
    let mut report = ActorReport::default();
    for node in nodes.values().filter_map(|n| *n) {
        let Some(site) = site(node) else {
            continue;
        };
        let fact = actor(node, &site, ACTOR_ALLOW_ASSUMPTION);
        if serde_json::to_string(&fact)
            .expect("overlay fact serializes")
            .len()
            > MAX_FACT_BYTES
        {
            report.oversized_endpoints += 1;
        } else {
            report.facts.push(fact);
        }
    }
    report
}

/// Full producer consistency check; this does not attest runtime policy/currency.
pub(crate) fn verify_actor<'a>(
    base: &'a RevisionedSnapshot,
    fact: &OverlayFact,
) -> Option<&'a Node> {
    if fact.cites.len() != 1 {
        return None;
    }
    let BaseRef::Node(id) = fact.cites.first()? else {
        return None;
    };
    let mut matches = base.graph.0.iter().filter(|n| &n.id == id);
    let endpoint = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let syntax = site(endpoint)?;
    (actor(endpoint, &syntax, ACTOR_ALLOW_ASSUMPTION) == *fact).then_some(endpoint)
}

/// Canonical verifier scratch using complete serialized base/candidate byte bounds.
pub(crate) fn verification_workspace(base_bytes: usize, actor_bytes: usize) -> Option<usize> {
    use crate::workbench::accounting::{json_bytes, slots, sum};
    let bytes = base_bytes.checked_add(actor_bytes)?;
    let value = json_bytes(bytes)?;
    // site(): endpoint provenance, typed site, checked provenance, cloned payload,
    // and the formatted syntax hash input overlap. No canonical fact exists yet.
    let site_validation = sum(&[
        value,
        value,
        value,
        value,
        bytes,
        std::mem::size_of::<PermissionSite>(),
        slots::<Provenance>(3)?,
    ])?;
    // actor(): retained typed site, props, typed Rule provenance and its JSON
    // conversion, plus content Vec/hash input and the complete comparison fact.
    let canonical = sum(&[
        value,
        value,
        value,
        value,
        bytes,
        std::mem::size_of::<PermissionSite>(),
        std::mem::size_of::<OverlayFact>(),
        std::mem::size_of::<Provenance>(),
    ])?;
    Some(site_validation.max(canonical))
}

#[cfg(test)]
pub(crate) mod tests;
