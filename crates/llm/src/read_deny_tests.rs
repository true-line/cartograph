use super::*;
use crate::{EgressFirewall, EgressPolicy, Embedding, LlmProvider, ProviderCaps, ProviderError};

fn read(path: &str) -> ReadIdentity {
    ReadIdentity {
        repo: "acme/shop".into(),
        path: path.into(),
        commit_sha: "abc123".into(),
        byte_start: 0,
        byte_end: 12,
    }
}

struct Fake(Locality);

impl LlmProvider for Fake {
    fn id(&self) -> &str {
        "fake:test"
    }
    fn locality(&self) -> Locality {
        self.0
    }
    fn embed(&self, _batch: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        Err(ProviderError::Unsupported("embeddings"))
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: false,
            chat: true,
            tool_use: false,
        }
    }
}

// AC-1409: every approved default pattern is refused toward a cloud provider,
// by file name anywhere in the tree or by trailing path, ignoring case.
#[test]
fn default_rules_refuse_secret_bearing_paths_for_cloud() {
    let rules = ReadDenyRules::default();
    for (path, rule) in [
        (".env", "env-file"),
        ("web/.env.production", "env-file"),
        ("deploy/prod.env", "env-file"),
        ("certs/server.pem", "key-material"),
        ("tls/site.KEY", "key-material"),
        ("a/b.p12", "key-material"),
        ("a/b.pfx", "key-material"),
        ("home/.ssh/id_rsa", "key-material"),
        ("home/.ssh/id_rsa.pub", "key-material"),
        ("id_ed25519", "key-material"),
        ("android/release.keystore", "key-material"),
        ("java/trust.jks", "key-material"),
        (".npmrc", "credential-file"),
        ("pkg/.pypirc", "credential-file"),
        (".netrc", "credential-file"),
        ("gcp/credentials.json", "credential-file"),
        ("infra/prod.tfvars", "credential-file"),
        ("home/.aws/credentials", "credential-file"),
        (".docker/config.json", "credential-file"),
    ] {
        assert_eq!(rules.matching_rule(path).as_deref(), Some(rule), "{path}");
    }
}

// AC-1409: ordinary files, near misses and templates outside the rules pass.
#[test]
fn ordinary_paths_and_near_misses_are_allowed() {
    let rules = ReadDenyRules::default();
    for path in [
        "src/orders.ts",
        "env.example",
        "env.sample",
        "docs/environment.md",
        "src/key.rs",
        "config.json",
        "app/config.json",
        "aws/credentials.md",
        "credentials",
        "src/envelope.ts",
        "keys/README",
    ] {
        assert_eq!(rules.matching_rule(path), None, "{path}");
    }
}

// AC-1409: a local provider is never refused; a cloud provider is refused
// only for a matching path, and the witness binds the exact read.
#[test]
fn refusal_needs_cloud_locality_and_a_matching_path() {
    let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([
        crate::AnalysisTier::Agentic,
    ]));
    let local = Fake(Locality::Local);
    let cloud = Fake(Locality::Cloud);
    assert_eq!(
        firewall.evaluate_read(&local, "attempt-1", &read(".env"), b"SECRET=1"),
        ReadDecision::Allowed
    );
    assert_eq!(
        firewall.evaluate_read(&cloud, "attempt-1", &read("src/orders.ts"), b"x"),
        ReadDecision::Allowed
    );
    let ReadDecision::Refused(witness) =
        firewall.evaluate_read(&cloud, "attempt-1", &read("web/.env"), b"SECRET=1")
    else {
        panic!("cloud read of .env must be refused");
    };
    assert_eq!(witness.recipe(), READ_DENY_RECIPE);
    assert_eq!(witness.rule_id(), "env-file");
    assert_eq!(witness.read(), &read("web/.env"));
    assert_eq!(witness.acquired_attempt_id(), "attempt-1");
    assert_eq!(
        witness.representation_hash(),
        blake3::hash(b"SECRET=1").to_hex().as_str()
    );
    // The witness keeps identities and the hash, never the withheld bytes.
    let wire = serde_json::to_string(&witness).unwrap();
    assert!(!wire.contains("SECRET"));
}

// AC-1409: project rules add to the defaults and cannot remove them; invalid
// or excess rules are refused.
#[test]
fn project_rules_only_add_and_are_validated() {
    let rules =
        ReadDenyRules::with_project_rules(["secrets/*.yaml".to_string(), "vault.txt".into()])
            .unwrap();
    assert_eq!(
        rules.matching_rule("ops/secrets/prod.yaml").as_deref(),
        Some("project:0")
    );
    assert_eq!(
        rules.matching_rule("vault.txt").as_deref(),
        Some("project:1")
    );
    assert_eq!(rules.matching_rule("ops/prod.yaml"), None);
    assert_eq!(rules.matching_rule(".env").as_deref(), Some("env-file"));
    let empty = ReadDenyRules::with_project_rules(Vec::new()).unwrap();
    assert_eq!(empty, ReadDenyRules::default());

    for bad in [
        "",
        "/etc/passwd",
        "a//b",
        "../x",
        "a/../b",
        "has space",
        "tab\t",
    ] {
        assert!(
            matches!(
                ReadDenyRules::with_project_rules([bad.to_string()]),
                Err(ReadDenyRuleError::Invalid(_))
            ),
            "{bad:?}"
        );
    }
    assert!(ReadDenyRules::with_project_rules(["x".repeat(MAX_RULE_BYTES + 1)]).is_err());
    let many = (0..=MAX_PROJECT_RULES).map(|index| format!("f{index}"));
    assert_eq!(
        ReadDenyRules::with_project_rules(many),
        Err(ReadDenyRuleError::TooMany)
    );
}

#[test]
fn component_glob_matches_whole_components_only() {
    assert!(matches_component("*.env", "prod.env"));
    assert!(matches_component("*.env", ".env"));
    assert!(!matches_component("*.env", "prod.envrc"));
    assert!(matches_component("id_rsa*", "id_rsa"));
    assert!(matches_component("a*b*c", "aXbYc"));
    assert!(!matches_component("a*b*c", "aXcYb"));
    assert!(!matches_component("ab*ba", "aba"));
    assert!(!matches_path(".aws/credentials", "credentials"));
    assert!(matches_path(".aws/credentials", "x/.aws/credentials"));
}
