//! Shared borrowed discovery policy for complete and bounded scoped readers.
use super::{ContextError, DiscoveryQuery, MatchKind, NAME_FIELDS};

/// Validated normalized query, independent of a complete graph snapshot.
/// Matching itself borrows stored fields and allocates no candidate strings.
#[derive(Debug, Clone)]
pub struct DiscoveryMatcher {
    query: DiscoveryQuery,
}

impl DiscoveryMatcher {
    /// Validate and normalize using the existing versioned discovery policy.
    pub fn new(query: &DiscoveryQuery) -> Result<Self, ContextError> {
        Ok(Self {
            query: query.normalized()?,
        })
    }

    /// Canonical query form to bind into the caller's scoped or complete basis.
    pub fn query(&self) -> &DiscoveryQuery {
        &self.query
    }

    /// Bind normalized query and exact label set using the complete discovery
    /// identity, after counting its input representation without copying it.
    /// The fixed digest is suitable for a scoped basis; it proves no visibility.
    pub fn query_digest(
        &self,
        labels: &std::collections::BTreeSet<String>,
    ) -> Result<[u8; 32], ContextError> {
        let input = super::QueryIdentity {
            policy: super::DISCOVERY_POLICY,
            query: &self.query,
            labels,
        };
        if let Err(required_bytes) = super::bounded_len(&input, super::MAX_RESPONSE_BYTES) {
            return Err(ContextError::ResponseBudgetExceeded {
                max_bytes: super::MAX_RESPONSE_BYTES,
                required_bytes,
            });
        }
        let identity = super::content_identity("discovery-query-v1", &input)?;
        Ok(*blake3::hash(identity.as_bytes()).as_bytes())
    }

    /// First match for an already admitted node, without provenance or visibility
    /// claims. Callers must apply their projection policy before disclosing it.
    pub fn match_node<'a>(
        &self,
        id: &'a str,
        properties: &'a serde_json::Value,
    ) -> Option<BorrowedDiscoveryMatch<'a>> {
        match_fact(&self.query, properties, id)
    }
}

/// A match borrowed from the snapshot; nothing is copied until the
/// candidate is admitted to a page.
#[derive(Clone, Copy)]
pub struct BorrowedDiscoveryMatch<'a> {
    /// The matching policy category.
    pub kind: MatchKind,
    /// First matching identity or name field.
    pub field: &'static str,
    /// Stored field value, borrowed without copying.
    pub value: &'a str,
}

/// Per-character Unicode lowercasing, the one mapping applied to both the
/// query and the compared fields. Unlike `str::to_lowercase` it has no
/// context rules (such as final sigma), so both sides always agree.
pub(super) fn lowercase(text: &str) -> impl Iterator<Item = char> + Clone + '_ {
    text.chars().flat_map(char::to_lowercase)
}

/// Whether `haystack` contains `needle` (already lowercased) after
/// lowercasing. Every position of the lowercased stream is a candidate start,
/// including scalars a single source character expands into; nothing is
/// allocated.
pub(super) fn contains_lowercase(haystack: &str, needle: &str) -> bool {
    let mut start = lowercase(haystack);
    loop {
        let mut rest = start.clone();
        if needle.chars().all(|wanted| rest.next() == Some(wanted)) {
            return true;
        }
        if start.next().is_none() {
            return false;
        }
    }
}

/// The first policy match for `fact`, if any.
fn match_fact<'a>(
    query: &DiscoveryQuery,
    properties: &'a serde_json::Value,
    id: &'a str,
) -> Option<BorrowedDiscoveryMatch<'a>> {
    let names = || {
        NAME_FIELDS
            .iter()
            .filter_map(|field| properties[*field].as_str().map(|value| (*field, value)))
    };
    let matched = |kind, (field, value)| BorrowedDiscoveryMatch { kind, field, value };
    match query {
        DiscoveryQuery::Identifier { id: wanted } => {
            (id == wanted).then(|| matched(MatchKind::ExactIdentifier, ("id", id)))
        }
        DiscoveryQuery::Name { name } => names()
            .find(|(_, value)| value == name)
            .map(|found| matched(MatchKind::ExactName, found)),
        DiscoveryQuery::Text { text } => std::iter::once(("id", id))
            .chain(names())
            .find(|(_, value)| contains_lowercase(value, text))
            .map(|found| matched(MatchKind::TextSubstring, found)),
    }
}
