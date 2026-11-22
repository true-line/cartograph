//! Target-specific read refusal, recipe `egress-read-deny/v1` (#679).
//!
//! The firewall refuses one acquired read representation, and allows the rest
//! of the bounded input, exactly when both hold:
//!
//! - the action's provider is [`Locality::Cloud`], and
//! - the representation's repo-relative path matches a deny rule.
//!
//! The default rules cannot be removed; a project may add rules. A local
//! provider never matches, because nothing leaves the machine. Redaction is a
//! separate state and never becomes a refusal.
//!
//! Only [`EgressFirewall::evaluate_read`](crate::EgressFirewall::evaluate_read)
//! constructs a [`ReadDenialWitness`]: it has no public constructor and does
//! not deserialize, so no model output, IPC request or stored row can build
//! one.

use crate::Locality;
use serde::Serialize;

/// The decision recipe this module implements. Bound into every witness.
pub const READ_DENY_RECIPE: &str = "egress-read-deny/v1";

/// At most this many project rules.
pub const MAX_PROJECT_RULES: usize = 64;
/// At most this many bytes in one project rule.
pub const MAX_RULE_BYTES: usize = 256;

/// One default rule: its stable id and patterns.
struct DefaultRule {
    id: &'static str,
    patterns: &'static [&'static str],
}

/// The default rules. A pattern without `/` matches the file name; a pattern
/// with `/` matches the trailing path components. `*` matches any run of
/// characters within one component. Matching ignores ASCII case.
const DEFAULT_RULES: [DefaultRule; 3] = [
    DefaultRule {
        id: "env-file",
        patterns: &[".env", ".env.*", "*.env"],
    },
    DefaultRule {
        id: "key-material",
        patterns: &[
            "*.pem",
            "*.key",
            "*.p12",
            "*.pfx",
            "id_rsa*",
            "id_ed25519*",
            "*.keystore",
            "*.jks",
        ],
    },
    DefaultRule {
        id: "credential-file",
        patterns: &[
            ".npmrc",
            ".pypirc",
            ".netrc",
            "credentials.json",
            "*.tfvars",
            ".aws/credentials",
            ".docker/config.json",
        ],
    },
];

/// The exact read a representation came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadIdentity {
    /// Repository the read is in.
    pub repo: String,
    /// Repo-relative path, `/`-separated.
    pub path: String,
    /// Commit the read is pinned to.
    pub commit_sha: String,
    /// First byte of the read range.
    pub byte_start: u64,
    /// One past the last byte of the read range.
    pub byte_end: u64,
}

/// Why a project rule was not accepted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadDenyRuleError {
    /// More than [`MAX_PROJECT_RULES`] rules.
    #[error("at most {MAX_PROJECT_RULES} read deny rules")]
    TooMany,
    /// A rule is empty, too long, absolute, has an empty or `..` component, or
    /// a character outside printable ASCII.
    #[error("invalid read deny rule: {0}")]
    Invalid(String),
}

/// The deny rules a firewall applies: the defaults plus project rules.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReadDenyRules {
    project: Vec<String>,
}

impl ReadDenyRules {
    /// The defaults plus `patterns`. Project rules only add to the defaults.
    pub fn with_project_rules(
        patterns: impl IntoIterator<Item = String>,
    ) -> Result<Self, ReadDenyRuleError> {
        let mut project = Vec::new();
        for pattern in patterns {
            if project.len() == MAX_PROJECT_RULES {
                return Err(ReadDenyRuleError::TooMany);
            }
            if !valid_pattern(&pattern) {
                return Err(ReadDenyRuleError::Invalid(pattern));
            }
            if !project.contains(&pattern) {
                project.push(pattern);
            }
        }
        Ok(Self { project })
    }

    /// The project rules, in the order given.
    pub fn project_rules(&self) -> &[String] {
        &self.project
    }

    /// The id of the first rule `path` matches: a default rule id, or
    /// `project:<index>` for a project rule.
    pub fn matching_rule(&self, path: &str) -> Option<String> {
        for rule in &DEFAULT_RULES {
            if rule
                .patterns
                .iter()
                .any(|pattern| matches_path(pattern, path))
            {
                return Some(rule.id.to_string());
            }
        }
        self.project
            .iter()
            .position(|pattern| matches_path(pattern, path))
            .map(|index| format!("project:{index}"))
    }

    pub(crate) fn evaluate(
        &self,
        locality: Locality,
        acquired_attempt_id: &str,
        read: &ReadIdentity,
        representation: &[u8],
    ) -> ReadDecision {
        if locality != Locality::Cloud {
            return ReadDecision::Allowed;
        }
        match self.matching_rule(&read.path) {
            None => ReadDecision::Allowed,
            Some(rule_id) => ReadDecision::Refused(ReadDenialWitness {
                recipe: READ_DENY_RECIPE,
                rule_id,
                read: read.clone(),
                representation_hash: blake3::hash(representation).to_hex().to_string(),
                acquired_attempt_id: acquired_attempt_id.to_string(),
            }),
        }
    }
}

/// What the firewall decided for one acquired read representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadDecision {
    /// The representation may be supplied (redaction still applies).
    Allowed,
    /// The representation is withheld; the witness records why.
    Refused(ReadDenialWitness),
}

/// Host-owned proof that the firewall refused one exact representation. Only
/// the firewall constructs it; it serializes for persistence and never
/// deserializes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadDenialWitness {
    recipe: &'static str,
    rule_id: String,
    read: ReadIdentity,
    representation_hash: String,
    acquired_attempt_id: String,
}

impl ReadDenialWitness {
    /// The decision recipe, [`READ_DENY_RECIPE`].
    pub fn recipe(&self) -> &'static str {
        self.recipe
    }
    /// The rule that matched.
    pub fn rule_id(&self) -> &str {
        &self.rule_id
    }
    /// The exact read the representation came from.
    pub fn read(&self) -> &ReadIdentity {
        &self.read
    }
    /// BLAKE3 hex of the refused representation bytes.
    pub fn representation_hash(&self) -> &str {
        &self.representation_hash
    }
    /// The completed acquired attempt the refusal refers to.
    pub fn acquired_attempt_id(&self) -> &str {
        &self.acquired_attempt_id
    }
}

fn valid_pattern(pattern: &str) -> bool {
    !pattern.is_empty()
        && pattern.len() <= MAX_RULE_BYTES
        && !pattern.starts_with('/')
        && pattern.bytes().all(|b| (0x21..0x7f).contains(&b))
        && pattern
            .split('/')
            .all(|component| !component.is_empty() && component != "..")
}

fn matches_path(pattern: &str, path: &str) -> bool {
    let wanted: Vec<&str> = pattern.split('/').collect();
    let have: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if have.len() < wanted.len() {
        return false;
    }
    wanted
        .iter()
        .zip(&have[have.len() - wanted.len()..])
        .all(|(pattern, component)| matches_component(pattern, component))
}

/// `*` matches any run of characters; everything else matches itself,
/// ignoring ASCII case.
fn matches_component(pattern: &str, component: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let component = component.to_ascii_lowercase();
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = component.strip_prefix(first) else {
        return false;
    };
    let tail: Vec<&str> = parts.collect();
    let Some((last, middle)) = tail.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

#[cfg(test)]
#[path = "read_deny_tests.rs"]
mod tests;
