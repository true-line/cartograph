//! Borrowed validation of the same serde provenance wire shape as core-prov.
use core_prov::{ConfidenceTier, ProducingMethod, Provenance, Tier};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub(super) struct Evidence<'a> {
    pub repo: &'a str,
    pub path: &'a str,
    pub byte_start: u64,
    pub byte_end: u64,
    pub commit_sha: &'a str,
}

// These fields are validated even though the security rule replaces the method.
#[derive(Default, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Method<'a> {
    Rule {
        #[serde(borrow, rename = "rule")]
        _rule: &'a str,
        #[serde(rename = "version")]
        _version: &'a str,
    },
    Model {
        #[serde(borrow, rename = "model")]
        _model: &'a str,
    },
    Human {
        #[serde(borrow, rename = "actor")]
        _actor: &'a str,
    },
    #[default]
    Unrecorded,
}

#[derive(Deserialize)]
pub(super) struct Support<'a> {
    pub tier: Tier,
    pub confidence_tier: ConfidenceTier,
    #[serde(borrow)]
    pub evidence: Vec<Evidence<'a>>,
    #[serde(borrow, rename = "extractor_id")]
    _extractor_id: &'a str,
    #[serde(rename = "content_hash")]
    _content_hash: &'a str,
    #[serde(default, borrow, rename = "method")]
    _method: Method<'a>,
}
impl<'a> Support<'a> {
    /// An endpoint's support: its `auth` fact's own provenance when it has
    /// one (#1128), else the endpoint's `prov`.
    pub fn from_endpoint_props(props: &'a serde_json::Value) -> Option<Self> {
        if props["auth"]["prov"].is_object() {
            return Self::from_props(&props["auth"]);
        }
        Self::from_props(props)
    }

    pub fn from_props(props: &'a serde_json::Value) -> Option<Self> {
        // A compacted `prov` is text with no escapes, so its strings borrow.
        let support: Self = match core_graph::stored::compact_text(&props["prov"]) {
            Some(text) => serde_json::from_str(text).ok()?,
            None => Self::deserialize(&props["prov"]).ok()?,
        };
        // Core's integrity validation checks the tier/ceiling. Required source
        // strings, evidence and method shape were validated by borrowed serde.
        let integrity = Provenance {
            tier: support.tier,
            confidence_tier: support.confidence_tier,
            evidence: Vec::new(),
            extractor_id: String::new(),
            content_hash: String::new(),
            method: ProducingMethod::Unrecorded,
            basis: None,
        };
        integrity.validate().ok()?;
        (support.confidence_tier != ConfidenceTier::Gap && !support.evidence.is_empty())
            .then_some(support)
    }
}
